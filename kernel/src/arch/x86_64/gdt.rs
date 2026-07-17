use core::cell::UnsafeCell;

use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5;
const PRIVILEGE_STACK_SIZE: usize = 64 * 1024;
const PRIVILEGE_STACK_TABLE_INDEX: usize = 0;

#[repr(align(16))]
struct Stack<const SIZE: usize>([u8; SIZE]);

static mut DOUBLE_FAULT_STACK: Stack<DOUBLE_FAULT_STACK_SIZE> = Stack([0; DOUBLE_FAULT_STACK_SIZE]);
static mut DEFAULT_PRIVILEGE_STACK: Stack<PRIVILEGE_STACK_SIZE> = Stack([0; PRIVILEGE_STACK_SIZE]);

struct TssCell(UnsafeCell<TaskStateSegment>);

unsafe impl Sync for TssCell {}

impl TssCell {
    fn new(tss: TaskStateSegment) -> Self {
        Self(UnsafeCell::new(tss))
    }

    fn get(&self) -> &TaskStateSegment {
        unsafe { &*self.0.get() }
    }

    unsafe fn set_privilege_stack(&self, stack_top: VirtAddr) {
        unsafe {
            (*self.0.get()).privilege_stack_table[PRIVILEGE_STACK_TABLE_INDEX] = stack_top;
        }
    }
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

lazy_static! {
    static ref TSS: TssCell = {
        let mut tss = TaskStateSegment::new();

        let double_fault_stack_start =
            VirtAddr::from_ptr(unsafe { &raw const DOUBLE_FAULT_STACK.0 });
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            double_fault_stack_start + DOUBLE_FAULT_STACK_SIZE as u64;
        tss.privilege_stack_table[PRIVILEGE_STACK_TABLE_INDEX] = default_privilege_stack_top();

        TssCell::new(tss)
    };
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();

        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(TSS.get()));

        (
            gdt,
            Selectors {
                code_selector,
                data_selector,
                user_code_selector,
                user_data_selector,
                tss_selector,
            },
        )
    };
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();

    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        SS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}

pub fn user_code_selector() -> u16 {
    GDT.1.user_code_selector.0 | 3
}

pub fn user_data_selector() -> u16 {
    GDT.1.user_data_selector.0 | 3
}

pub fn set_privilege_stack(stack_top: VirtAddr) {
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        TSS.set_privilege_stack(stack_top);
    });
}

pub fn reset_privilege_stack() {
    set_privilege_stack(default_privilege_stack_top());
}

fn default_privilege_stack_top() -> VirtAddr {
    let stack_start = VirtAddr::from_ptr(unsafe { &raw const DEFAULT_PRIVILEGE_STACK.0 });
    stack_start + PRIVILEGE_STACK_SIZE as u64
}
