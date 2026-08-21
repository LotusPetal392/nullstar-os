//! Low-memory application-processor trampoline for x86_64 SMP startup.
//!
//! The trampoline is linked into the kernel as position-independent bytes,
//! copied into one reserved usable page below 1 MiB, identity-mapped in the
//! bootstrap page tables, and patched with the current CR3 plus the AP's Rust
//! entry stack and arguments. An AP enters it in 16-bit real mode after SIPI,
//! transitions through protected mode, enables long mode with the BSP's page
//! tables, switches to its kernel stack, and jumps to the high-half Rust entry.

use core::{
    arch::global_asm,
    ptr,
    sync::atomic::{AtomicU64, Ordering, fence},
};

use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB, Translate,
    },
};

use crate::memory::BootInfoFrameAllocator;

const PAGE_SIZE: usize = 4096;
const LOW_MEMORY_LIMIT: u64 = 0x10_0000;

static INSTALLED_TRAMPOLINE_PHYSICAL_ADDRESS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrampolineError {
    InvalidPhysicalAddress,
    ImageTooLarge,
    Cr3AboveFourGiB,
    MappingFailed,
}

#[derive(Clone, Copy, Debug)]
pub struct ApTrampoline {
    physical_address: u64,
    startup_vector: u8,
}

impl ApTrampoline {
    pub fn install(
        frame: PhysFrame<Size4KiB>,
        physical_memory_offset: VirtAddr,
        mapper: &mut OffsetPageTable<'static>,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> Result<Self, TrampolineError> {
        let physical_address = frame.start_address().as_u64();
        if physical_address < 0x1000
            || physical_address >= LOW_MEMORY_LIMIT
            || physical_address & (PAGE_SIZE as u64 - 1) != 0
        {
            return Err(TrampolineError::InvalidPhysicalAddress);
        }

        let startup_vector = u8::try_from(physical_address >> 12)
            .map_err(|_| TrampolineError::InvalidPhysicalAddress)?;
        if startup_vector == 0 {
            return Err(TrampolineError::InvalidPhysicalAddress);
        }

        let image_len = image_len();
        if image_len == 0 || image_len > PAGE_SIZE {
            return Err(TrampolineError::ImageTooLarge);
        }

        let identity_address = VirtAddr::new(physical_address);
        let identity_page = Page::<Size4KiB>::containing_address(identity_address);
        if mapper.translate_addr(identity_address) != Some(PhysAddr::new(physical_address)) {
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            unsafe { mapper.map_to(identity_page, frame, flags, frame_allocator) }
                .map_err(|_| TrampolineError::MappingFailed)?
                .flush();
        }

        let destination = mapped_pointer(physical_memory_offset, physical_address)?;
        unsafe {
            ptr::write_bytes(destination, 0, PAGE_SIZE);
            ptr::copy_nonoverlapping(image_start(), destination, image_len);
        }

        let base = u32::try_from(physical_address)
            .map_err(|_| TrampolineError::InvalidPhysicalAddress)?;
        write_u32(destination, offset_of_gdt_pointer_base(), physical_address as u32);
        patch_segment_base(destination, offset_of_code32_descriptor(), base);
        patch_segment_base(destination, offset_of_data32_descriptor(), base);
        write_u32(
            destination,
            offset_of_long_mode_jump_target(),
            base.saturating_add(offset_of_long_mode_entry() as u32),
        );
        write_u32(destination, offset_of_parameter_base(), base);
        fence(Ordering::SeqCst);

        Ok(Self {
            physical_address,
            startup_vector,
        })
    }

    pub fn install_global(
        frame: PhysFrame<Size4KiB>,
        physical_memory_offset: VirtAddr,
        mapper: &mut OffsetPageTable<'static>,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> Result<Self, TrampolineError> {
        let trampoline = Self::install(frame, physical_memory_offset, mapper, frame_allocator)?;
        INSTALLED_TRAMPOLINE_PHYSICAL_ADDRESS
            .store(trampoline.physical_address, Ordering::Release);
        Ok(trampoline)
    }

    pub fn installed() -> Option<Self> {
        let physical_address = INSTALLED_TRAMPOLINE_PHYSICAL_ADDRESS.load(Ordering::Acquire);
        if physical_address == 0 {
            return None;
        }
        Some(Self {
            physical_address,
            startup_vector: (physical_address >> 12) as u8,
        })
    }

    pub const fn physical_address(self) -> u64 {
        self.physical_address
    }

    pub const fn startup_vector(self) -> u8 {
        self.startup_vector
    }

    pub fn configure(
        self,
        physical_memory_offset: VirtAddr,
        cpu_index: u32,
        apic_id: u32,
        stack_top: VirtAddr,
        entry: VirtAddr,
    ) -> Result<(), TrampolineError> {
        let (level_4_frame, _) = Cr3::read();
        let cr3 = level_4_frame.start_address().as_u64();
        let cr3 = u32::try_from(cr3).map_err(|_| TrampolineError::Cr3AboveFourGiB)?;
        let destination = mapped_pointer(physical_memory_offset, self.physical_address)?;

        write_u32(destination, offset_of_parameter_cr3(), cr3);
        write_u32(destination, offset_of_parameter_cpu(), cpu_index);
        write_u32(destination, offset_of_parameter_apic(), apic_id);
        write_u64(destination, offset_of_parameter_stack(), stack_top.as_u64());
        write_u64(destination, offset_of_parameter_entry(), entry.as_u64());
        fence(Ordering::SeqCst);
        Ok(())
    }
}

fn mapped_pointer(
    physical_memory_offset: VirtAddr,
    physical_address: u64,
) -> Result<*mut u8, TrampolineError> {
    let address = physical_memory_offset
        .as_u64()
        .checked_add(physical_address)
        .ok_or(TrampolineError::InvalidPhysicalAddress)?;
    let address = usize::try_from(address).map_err(|_| TrampolineError::InvalidPhysicalAddress)?;
    Ok(address as *mut u8)
}

fn patch_segment_base(destination: *mut u8, descriptor_offset: usize, base: u32) {
    unsafe {
        ptr::write(destination.add(descriptor_offset + 2), base as u8);
        ptr::write(destination.add(descriptor_offset + 3), (base >> 8) as u8);
        ptr::write(destination.add(descriptor_offset + 4), (base >> 16) as u8);
        ptr::write(destination.add(descriptor_offset + 7), (base >> 24) as u8);
    }
}

fn write_u32(destination: *mut u8, offset: usize, value: u32) {
    unsafe { ptr::write_unaligned(destination.add(offset).cast::<u32>(), value) };
}

fn write_u64(destination: *mut u8, offset: usize, value: u64) {
    unsafe { ptr::write_unaligned(destination.add(offset).cast::<u64>(), value) };
}

fn image_start() -> *const u8 {
    unsafe { &raw const __nullstar_ap_trampoline_start }
}

fn image_len() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_end })
}

fn offset_of_code32_descriptor() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_gdt_code32 })
}

fn offset_of_data32_descriptor() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_gdt_data32 })
}

fn offset_of_gdt_pointer_base() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_gdt_pointer_base })
}

fn offset_of_long_mode_jump_target() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_long_jump_target })
}

fn offset_of_long_mode_entry() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_long_mode })
}

fn offset_of_parameter_cr3() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_cr3 })
}

fn offset_of_parameter_cpu() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_cpu })
}

fn offset_of_parameter_apic() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_apic })
}

fn offset_of_parameter_base() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_base })
}

fn offset_of_parameter_stack() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_stack })
}

fn offset_of_parameter_entry() -> usize {
    symbol_offset(unsafe { &raw const __nullstar_ap_trampoline_parameter_entry })
}

fn symbol_offset(symbol: *const u8) -> usize {
    (symbol as usize).saturating_sub(image_start() as usize)
}

unsafe extern "C" {
    static __nullstar_ap_trampoline_start: u8;
    static __nullstar_ap_trampoline_end: u8;
    static __nullstar_ap_trampoline_gdt_code32: u8;
    static __nullstar_ap_trampoline_gdt_data32: u8;
    static __nullstar_ap_trampoline_gdt_pointer_base: u8;
    static __nullstar_ap_trampoline_long_jump_target: u8;
    static __nullstar_ap_trampoline_long_mode: u8;
    static __nullstar_ap_trampoline_parameter_cr3: u8;
    static __nullstar_ap_trampoline_parameter_cpu: u8;
    static __nullstar_ap_trampoline_parameter_apic: u8;
    static __nullstar_ap_trampoline_parameter_base: u8;
    static __nullstar_ap_trampoline_parameter_stack: u8;
    static __nullstar_ap_trampoline_parameter_entry: u8;
}

global_asm!(
    r#"
    .section .rodata.ap_trampoline, "a", @progbits
    .balign 16
    .code16
    .global __nullstar_ap_trampoline_start
__nullstar_ap_trampoline_start:
    cli
    cld
    movw %cs, %ax
    movw %ax, %ds
    movw %ax, %es
    lgdtl %cs:(__nullstar_ap_trampoline_gdt_pointer - __nullstar_ap_trampoline_start)

    movl %cr0, %eax
    orl $0x1, %eax
    movl %eax, %cr0
    ljmp $0x08, $(__nullstar_ap_trampoline_protected_mode - __nullstar_ap_trampoline_start)

    .code32
__nullstar_ap_trampoline_protected_mode:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    movl (__nullstar_ap_trampoline_parameter_base - __nullstar_ap_trampoline_start), %ebx
    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4
    movl (__nullstar_ap_trampoline_parameter_cr3 - __nullstar_ap_trampoline_start), %eax
    movl %eax, %cr3

    movl $0xc0000080, %ecx
    rdmsr
    orl $0x100, %eax
    wrmsr

    movl %cr0, %eax
    orl $0x80000000, %eax
    movl %eax, %cr0
    ljmpl *(__nullstar_ap_trampoline_long_jump - __nullstar_ap_trampoline_start)

    .code64
    .global __nullstar_ap_trampoline_long_mode
__nullstar_ap_trampoline_long_mode:
    movl %ebx, %ebx
    movw $0x20, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    movq (__nullstar_ap_trampoline_parameter_stack - __nullstar_ap_trampoline_start)(%rbx), %rsp
    andq $-16, %rsp
    subq $8, %rsp
    xorq %rbp, %rbp
    movl (__nullstar_ap_trampoline_parameter_cpu - __nullstar_ap_trampoline_start)(%rbx), %edi
    movl (__nullstar_ap_trampoline_parameter_apic - __nullstar_ap_trampoline_start)(%rbx), %esi
    movq (__nullstar_ap_trampoline_parameter_entry - __nullstar_ap_trampoline_start)(%rbx), %rax
    jmp *%rax

    .balign 8
__nullstar_ap_trampoline_long_jump:
    .global __nullstar_ap_trampoline_long_jump_target
__nullstar_ap_trampoline_long_jump_target:
    .long 0
    .word 0x18

    .balign 8
__nullstar_ap_trampoline_gdt:
    .quad 0x0000000000000000
    .global __nullstar_ap_trampoline_gdt_code32
__nullstar_ap_trampoline_gdt_code32:
    .quad 0x00cf9a000000ffff
    .global __nullstar_ap_trampoline_gdt_data32
__nullstar_ap_trampoline_gdt_data32:
    .quad 0x00cf92000000ffff
    .quad 0x00af9a000000ffff
    .quad 0x00cf92000000ffff
__nullstar_ap_trampoline_gdt_end:

__nullstar_ap_trampoline_gdt_pointer:
    .word __nullstar_ap_trampoline_gdt_end - __nullstar_ap_trampoline_gdt - 1
    .global __nullstar_ap_trampoline_gdt_pointer_base
__nullstar_ap_trampoline_gdt_pointer_base:
    .long 0

    .balign 8
    .global __nullstar_ap_trampoline_parameter_cr3
__nullstar_ap_trampoline_parameter_cr3:
    .long 0
    .global __nullstar_ap_trampoline_parameter_cpu
__nullstar_ap_trampoline_parameter_cpu:
    .long 0
    .global __nullstar_ap_trampoline_parameter_apic
__nullstar_ap_trampoline_parameter_apic:
    .long 0
    .global __nullstar_ap_trampoline_parameter_base
__nullstar_ap_trampoline_parameter_base:
    .long 0
    .global __nullstar_ap_trampoline_parameter_stack
__nullstar_ap_trampoline_parameter_stack:
    .quad 0
    .global __nullstar_ap_trampoline_parameter_entry
__nullstar_ap_trampoline_parameter_entry:
    .quad 0

    .global __nullstar_ap_trampoline_end
__nullstar_ap_trampoline_end:
    .code64
    "#,
    options(att_syntax)
);
