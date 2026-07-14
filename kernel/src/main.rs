#![no_std]
#![no_main]

use bootloader_api::{
    BootInfo, entry_point,
    info::{FrameBufferInfo, PixelFormat},
};
use core::panic::PanicInfo;
use core::ptr;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster};

mod serial;

entry_point!(kernel_main);

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
    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        halt_forever();
    };
    let info = framebuffer.info();
    let mut writer = FramebufferWriter::new(framebuffer.buffer_mut(), info);

    writer.write_string("GalacticOS\n");
    writer.write_string("-------------\n\n");
    writer.write_string("The x86-64 kernel has booted successfully.\n");
    writer.write_string("Rust is writing directly to the framebuffer.");

    serial_println!("kernel entered kernel_main");

    halt_forever();
}

fn halt_forever() -> ! {
    loop {
        // Halt until an interrupt occurs. Interrupts are not configured yet,
        // so this effectively leaves the processor stopped.
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    halt_forever();
}
