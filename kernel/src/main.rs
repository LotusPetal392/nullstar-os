#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{
    BootInfo, BootloaderConfig,
    config::Mapping,
    entry_point,
    info::{FrameBufferInfo, PixelFormat},
};
use core::{alloc::Layout, panic::PanicInfo, ptr};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster};
use x86_64::VirtAddr;

mod allocator;
mod gdt;
mod interrupts;
mod keyboard;
mod memory;
mod serial;

static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
const LETTER_SPACING: usize = 2;
const LINE_SPACING: usize = 2;
const BORDER_PADDING: usize = 8;

#[derive(Clone, Copy)]
struct Color {
    red: u8,
    green: u8,
    blue: u8,
}

const FOREGROUND: Color = Color {
    red: 110,
    green: 235,
    blue: 255,
};

struct FramebufferWriter<'a> {
    buffer: &'a mut [u8],
    info: FrameBufferInfo,
    x: usize,
    y: usize,
}

impl<'a> FramebufferWriter<'a> {
    fn new(buffer: &'a mut [u8], info: FrameBufferInfo) -> Self {
        let mut writer = Self {
            buffer,
            info,
            x: BORDER_PADDING,
            y: BORDER_PADDING,
        };
        writer.clear_screen();
        writer
    }

    fn clear_screen(&mut self) {
        for byte in self.buffer.iter_mut() {
            // The framebuffer is memory-mapped I/O, so writes must be volatile.
            unsafe { ptr::write_volatile(byte, 0) };
        }
        self.x = BORDER_PADDING;
        self.y = BORDER_PADDING;
    }

    fn write_string(&mut self, text: &str) {
        for character in text.chars() {
            self.write_char(character);
        }
    }

    fn write_char(&mut self, character: char) {
        match character {
            '\n' => {
                self.new_line();
                return;
            }
            '\r' => {
                self.x = BORDER_PADDING;
                return;
            }
            _ => {}
        }

        let raster = get_raster(character, FONT_WEIGHT, FONT_HEIGHT)
            .or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT))
            .expect("the fallback glyph must be available");

        if self.x + raster.width() > self.info.width {
            self.new_line();
        }

        for (row, pixels) in raster.raster().iter().enumerate() {
            for (column, intensity) in pixels.iter().copied().enumerate() {
                let color = Color {
                    red: scale_channel(FOREGROUND.red, intensity),
                    green: scale_channel(FOREGROUND.green, intensity),
                    blue: scale_channel(FOREGROUND.blue, intensity),
                };
                self.write_pixel(self.x + column, self.y + row, color);
            }
        }

        self.x += raster.width() + LETTER_SPACING;
    }

    fn new_line(&mut self) {
        let line_height = FONT_HEIGHT.val() + LINE_SPACING;
        self.x = BORDER_PADDING;
        self.y += line_height;

        if self.y + FONT_HEIGHT.val() > self.info.height {
            self.scroll_up(line_height);
            self.y = self.y.saturating_sub(line_height);
        }
    }

    fn scroll_up(&mut self, rows: usize) {
        let Some(bytes_per_row) = self.info.stride.checked_mul(self.info.bytes_per_pixel) else {
            self.clear_screen();
            return;
        };
        let Some(shift) = rows.checked_mul(bytes_per_row) else {
            self.clear_screen();
            return;
        };

        if shift >= self.buffer.len() {
            self.clear_screen();
            return;
        }

        let remaining = self.buffer.len() - shift;
        for destination in 0..remaining {
            // Copy forwards because the source starts after the destination.
            let value =
                unsafe { ptr::read_volatile(self.buffer.as_ptr().add(destination + shift)) };
            unsafe { ptr::write_volatile(self.buffer.as_mut_ptr().add(destination), value) };
        }
        for byte in &mut self.buffer[remaining..] {
            unsafe { ptr::write_volatile(byte, 0) };
        }
    }

    fn write_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let Some(pixel_index) = y
            .checked_mul(self.info.stride)
            .and_then(|row| row.checked_add(x))
            .and_then(|pixel| pixel.checked_mul(self.info.bytes_per_pixel))
        else {
            return;
        };
        let Some(pixel_end) = pixel_index.checked_add(self.info.bytes_per_pixel) else {
            return;
        };
        if pixel_end > self.buffer.len() {
            return;
        }

        let unknown_pixel = match self.info.pixel_format {
            PixelFormat::Unknown {
                red_position,
                green_position,
                blue_position,
            } => {
                channel_at(color.red, red_position)
                    | channel_at(color.green, green_position)
                    | channel_at(color.blue, blue_position)
            }
            _ => 0,
        };

        for byte_offset in 0..self.info.bytes_per_pixel {
            let value = match self.info.pixel_format {
                PixelFormat::Rgb => match byte_offset {
                    0 => color.red,
                    1 => color.green,
                    2 => color.blue,
                    _ => 0,
                },
                PixelFormat::Bgr => match byte_offset {
                    0 => color.blue,
                    1 => color.green,
                    2 => color.red,
                    _ => 0,
                },
                PixelFormat::U8 => {
                    if byte_offset == 0 {
                        grayscale(color)
                    } else {
                        0
                    }
                }
                PixelFormat::Unknown { .. } => unknown_pixel
                    .to_le_bytes()
                    .get(byte_offset)
                    .copied()
                    .unwrap_or(0),
                _ => 0,
            };

            unsafe {
                ptr::write_volatile(
                    self.buffer.as_mut_ptr().add(pixel_index + byte_offset),
                    value,
                )
            };
        }
    }
}

fn scale_channel(channel: u8, intensity: u8) -> u8 {
    ((u16::from(channel) * u16::from(intensity)) / 255) as u8
}

fn grayscale(color: Color) -> u8 {
    ((u16::from(color.red) * 77 + u16::from(color.green) * 150 + u16::from(color.blue) * 29) >> 8)
        as u8
}

fn channel_at(channel: u8, position: u8) -> u64 {
    u64::from(channel)
        .checked_shl(u32::from(position))
        .unwrap_or(0)
}

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let Some(physical_memory_offset) = boot_info.physical_memory_offset.into_option() else {
        serial_println!("no physical memory mapping was provided by the bootloader");
        hlt_loop();
    };
    let physical_memory_offset = VirtAddr::new(physical_memory_offset);

    let mut mapper = unsafe { memory::init(physical_memory_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_regions) };

    if let Err(error) = allocator::init_heap(&mut mapper, &mut frame_allocator) {
        serial_println!("failed to initialize the kernel heap: {error:?}");
        hlt_loop();
    }

    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        serial_println!("no framebuffer was provided by the bootloader");
        hlt_loop();
    };
    let info = framebuffer.info();
    let mut writer = FramebufferWriter::new(framebuffer.buffer_mut(), info);

    gdt::init();
    interrupts::init();
    heap_allocation_self_test();

    writer.write_string("GalacticOS\n");
    writer.write_string("-------------\n\n");
    writer.write_string("The x86-64 kernel has booted successfully.\n");
    writer.write_string("Rust is writing directly to the framebuffer.\n\n");

    writer.write_string("Physical memory manager ready\n");
    writer.write_string("Kernel heap ready\n");
    writer.write_string("Heap allocation self-test passed\n");
    writer.write_string("GDT loaded\n");
    writer.write_string("IDT loaded\n");
    writer.write_string("Interrupts enabled\n");

    writer.write_string("Keyboard ready. Type below:\n");

    let usable_frames = frame_allocator.usable_frame_count();
    let usable_mebibytes = usable_frames.saturating_mul(memory::FRAME_SIZE) / (1024 * 1024);
    serial_println!(
        "physical memory manager initialized: offset={:#x}, usable_frames={}, usable_memory={} MiB",
        physical_memory_offset.as_u64(),
        usable_frames,
        usable_mebibytes
    );
    serial_println!(
        "kernel heap initialized: start={:#x}, size={} KiB, pages={}, allocated_frames={}, remaining_frames={}",
        allocator::HEAP_START,
        allocator::HEAP_SIZE / 1024,
        allocator::HEAP_PAGE_COUNT,
        frame_allocator.allocated_frame_count(),
        frame_allocator.remaining_frame_count()
    );
    serial_println!("kernel entered kernel_main");

    let mut reported_seconds = 0;
    loop {
        x86_64::instructions::hlt();

        while let Some(key) = keyboard::poll_key() {
            match key {
                pc_keyboard::DecodedKey::Unicode(character) => {
                    writer.write_char(character);
                    serial_print!("{character}");
                }
                pc_keyboard::DecodedKey::RawKey(key_code) => {
                    serial_print!("<{key_code:?}>");
                }
            }
        }

        let elapsed_seconds = interrupts::timer_ticks() / interrupts::TIMER_HZ;
        if elapsed_seconds > reported_seconds {
            reported_seconds = elapsed_seconds;
            serial_println!("uptime: {elapsed_seconds}s");
        }
    }
}

fn heap_allocation_self_test() {
    const HEAP_VALUE: u64 = 0xC0FF_EE00_D15C_A11C;
    const VECTOR_LENGTH: u64 = 1024;
    const EXPECTED_SUM: u64 = (VECTOR_LENGTH - 1) * VECTOR_LENGTH / 2;

    let heap_value = Box::new(HEAP_VALUE);
    let mut values = Vec::new();
    for value in 0..VECTOR_LENGTH {
        values.push(value);
    }

    assert_eq!(*heap_value, HEAP_VALUE);
    assert_eq!(values.len(), VECTOR_LENGTH as usize);
    assert_eq!(values.iter().copied().sum::<u64>(), EXPECTED_SUM);

    drop(values);
    drop(heap_value);

    let reused_value = Box::new(0xA110_C8ED_u64);
    assert_eq!(*reused_value, 0xA110_C8ED);

    serial_println!(
        "heap allocation self-test passed: vector_len={}, vector_sum={}",
        VECTOR_LENGTH,
        EXPECTED_SUM
    );
}

fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[alloc_error_handler]
fn allocation_error(layout: Layout) -> ! {
    serial_println!("KERNEL ALLOCATION ERROR: {layout:?}");
    hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {info}");
    hlt_loop();
}
