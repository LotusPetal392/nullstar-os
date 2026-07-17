use alloc::vec::Vec;
use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::{fmt, mem::size_of, ptr};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster};
use spin::Mutex;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
const LETTER_SPACING: usize = 2;
const LINE_SPACING: usize = 2;
const BORDER_PADDING: usize = 8;
const FLUSH_WORD_BYTES: usize = size_of::<u64>();

static CONSOLE: Mutex<Option<FramebufferWriter>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    AlreadyInitialized,
    InvalidGeometry,
    ShadowAllocationFailed,
    ShadowScrollSelfTestFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub shadow_bytes: usize,
    pub text_columns: usize,
    pub text_rows: usize,
    pub scrolls: u64,
    pub flush_operations: u64,
    pub flushed_bytes: u64,
}

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

pub fn init(framebuffer: FrameBuffer) -> Result<(), InitError> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut console = CONSOLE.lock();
        if console.is_some() {
            return Err(InitError::AlreadyInitialized);
        }
        if !verify_shadow_scroll_algorithm() {
            return Err(InitError::ShadowScrollSelfTestFailed);
        }

        let info = framebuffer.info();
        let buffer = framebuffer.into_buffer();
        let writer = FramebufferWriter::new(buffer, info)?;
        let stats = writer.stats();
        *console = Some(writer);

        crate::serial_println!(
            "framebuffer shadow buffer initialized: width={}, height={}, stride={}, bpp={}, shadow_bytes={}, flush_word_bytes={}, scroll_test=passed",
            stats.width,
            stats.height,
            stats.stride,
            stats.bytes_per_pixel,
            stats.shadow_bytes,
            FLUSH_WORD_BYTES
        );

        Ok(())
    })
}

pub fn clear() {
    with_console(|console| console.clear_screen());
}

pub fn write_char(character: char) {
    with_console(|console| console.write_char(character));
}

pub fn backspace() {
    with_console(|console| console.backspace());
}

pub fn text_columns() -> Option<usize> {
    with_console(|console| console.text_columns())
}

pub fn stats() -> Option<Stats> {
    with_console(|console| console.stats())
}

#[doc(hidden)]
pub fn _print(arguments: fmt::Arguments<'_>) {
    use core::fmt::Write;

    with_console(|console| {
        let _ = console.write_fmt(arguments);
    });
}

fn with_console<R>(operation: impl FnOnce(&mut FramebufferWriter) -> R) -> Option<R> {
    x86_64::instructions::interrupts::without_interrupts(|| CONSOLE.lock().as_mut().map(operation))
}

struct FramebufferWriter {
    framebuffer: &'static mut [u8],
    shadow: Vec<u8>,
    info: FrameBufferInfo,
    visible_bytes: usize,
    bytes_per_row: usize,
    x: usize,
    y: usize,
    scrolls: u64,
    flush_operations: u64,
    flushed_bytes: u64,
}

impl FramebufferWriter {
    fn new(
        framebuffer: &'static mut [u8],
        info: FrameBufferInfo,
    ) -> Result<Self, InitError> {
        if info.width == 0
            || info.height == 0
            || info.bytes_per_pixel == 0
            || info.stride < info.width
        {
            return Err(InitError::InvalidGeometry);
        }

        let bytes_per_row = info
            .stride
            .checked_mul(info.bytes_per_pixel)
            .ok_or(InitError::InvalidGeometry)?;
        let visible_bytes = bytes_per_row
            .checked_mul(info.height)
            .ok_or(InitError::InvalidGeometry)?;
        if visible_bytes > framebuffer.len() {
            return Err(InitError::InvalidGeometry);
        }

        let mut shadow = Vec::new();
        shadow
            .try_reserve_exact(visible_bytes)
            .map_err(|_| InitError::ShadowAllocationFailed)?;
        shadow.resize(visible_bytes, 0);

        let mut writer = Self {
            framebuffer,
            shadow,
            info,
            visible_bytes,
            bytes_per_row,
            x: BORDER_PADDING,
            y: BORDER_PADDING,
            scrolls: 0,
            flush_operations: 0,
            flushed_bytes: 0,
        };
        writer.clear_screen();
        Ok(writer)
    }

    fn stats(&self) -> Stats {
        Stats {
            width: self.info.width,
            height: self.info.height,
            stride: self.info.stride,
            bytes_per_pixel: self.info.bytes_per_pixel,
            shadow_bytes: self.shadow.len(),
            text_columns: self.text_columns(),
            text_rows: self.text_rows(),
            scrolls: self.scrolls,
            flush_operations: self.flush_operations,
            flushed_bytes: self.flushed_bytes,
        }
    }

    fn clear_screen(&mut self) {
        self.shadow.fill(0);
        self.flush_all();
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

        let glyph_x = self.x;
        let glyph_y = self.y;
        for (row, pixels) in raster.raster().iter().enumerate() {
            for (column, intensity) in pixels.iter().copied().enumerate() {
                let color = Color {
                    red: scale_channel(FOREGROUND.red, intensity),
                    green: scale_channel(FOREGROUND.green, intensity),
                    blue: scale_channel(FOREGROUND.blue, intensity),
                };
                self.write_shadow_pixel(glyph_x + column, glyph_y + row, color);
            }
        }
        self.flush_rectangle(glyph_x, glyph_y, raster.width(), raster.raster().len());

        self.x += raster.width() + LETTER_SPACING;
    }

    fn backspace(&mut self) {
        if self.x <= BORDER_PADDING {
            return;
        }

        let character_width = glyph_advance(' ');
        let new_x = self.x.saturating_sub(character_width).max(BORDER_PADDING);
        let erase_width = self.x.saturating_sub(new_x);

        self.clear_rectangle(new_x, self.y, erase_width, FONT_HEIGHT.val());
        self.x = new_x;
    }

    fn text_columns(&self) -> usize {
        let available_width = self.info.width.saturating_sub(BORDER_PADDING * 2);
        available_width / glyph_advance(' ')
    }

    fn text_rows(&self) -> usize {
        let available_height = self.info.height.saturating_sub(BORDER_PADDING * 2);
        available_height / (FONT_HEIGHT.val() + LINE_SPACING)
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
        let top_row = BORDER_PADDING.min(self.info.height);
        if !scroll_shadow_rows(
            &mut self.shadow,
            self.bytes_per_row,
            top_row,
            self.info.height,
            rows,
        ) {
            self.clear_screen();
            return;
        }

        self.scrolls = self.scrolls.saturating_add(1);
        self.flush_rows(top_row, self.info.height);
    }

    fn clear_rectangle(&mut self, x: usize, y: usize, width: usize, height: usize) {
        let end_x = x.saturating_add(width).min(self.info.width);
        let end_y = y.saturating_add(height).min(self.info.height);
        if x >= end_x || y >= end_y {
            return;
        }

        for pixel_y in y..end_y {
            let Some(start) = self.byte_offset(x, pixel_y) else {
                return;
            };
            let Some(end) = self.byte_offset(end_x, pixel_y) else {
                return;
            };
            self.shadow[start..end].fill(0);
        }
        self.flush_rectangle(x, y, end_x - x, end_y - y);
    }

    fn write_shadow_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let Some(pixel_index) = self.byte_offset(x, y) else {
            return;
        };
        let Some(pixel_end) = pixel_index.checked_add(self.info.bytes_per_pixel) else {
            return;
        };
        if pixel_end > self.visible_bytes {
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
            self.shadow[pixel_index + byte_offset] = value;
        }
    }

    fn byte_offset(&self, x: usize, y: usize) -> Option<usize> {
        y.checked_mul(self.info.stride)
            .and_then(|row| row.checked_add(x))
            .and_then(|pixel| pixel.checked_mul(self.info.bytes_per_pixel))
            .filter(|offset| *offset <= self.visible_bytes)
    }

    fn flush_all(&mut self) {
        self.flush_contiguous_range(0, self.visible_bytes);
        self.record_flush(self.visible_bytes);
    }

    fn flush_rows(&mut self, start_row: usize, end_row: usize) {
        let start_row = start_row.min(self.info.height);
        let end_row = end_row.min(self.info.height);
        if start_row >= end_row {
            return;
        }

        let start = start_row * self.bytes_per_row;
        let end = end_row * self.bytes_per_row;
        self.flush_contiguous_range(start, end);
        self.record_flush(end - start);
    }

    fn flush_rectangle(&mut self, x: usize, y: usize, width: usize, height: usize) {
        let end_x = x.saturating_add(width).min(self.info.width);
        let end_y = y.saturating_add(height).min(self.info.height);
        if x >= end_x || y >= end_y {
            return;
        }

        let mut flushed = 0usize;
        for pixel_y in y..end_y {
            let Some(start) = self.byte_offset(x, pixel_y) else {
                return;
            };
            let Some(end) = self.byte_offset(end_x, pixel_y) else {
                return;
            };
            self.flush_contiguous_range(start, end);
            flushed = flushed.saturating_add(end - start);
        }
        self.record_flush(flushed);
    }

    fn flush_contiguous_range(&mut self, start: usize, end: usize) {
        if start >= end || end > self.visible_bytes || end > self.framebuffer.len() {
            return;
        }

        let length = end - start;
        let source = unsafe { self.shadow.as_ptr().add(start) };
        let destination = unsafe { self.framebuffer.as_mut_ptr().add(start) };
        unsafe { volatile_copy(destination, source, length) };
    }

    fn record_flush(&mut self, bytes: usize) {
        self.flush_operations = self.flush_operations.saturating_add(1);
        self.flushed_bytes = self.flushed_bytes.saturating_add(bytes as u64);
    }
}

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_string(text);
        Ok(())
    }
}

fn scroll_shadow_rows(
    shadow: &mut [u8],
    bytes_per_row: usize,
    top_row: usize,
    bottom_row: usize,
    rows: usize,
) -> bool {
    if bytes_per_row == 0 || top_row >= bottom_row || rows == 0 || rows >= bottom_row - top_row {
        return false;
    }

    let Some(destination_start) = top_row.checked_mul(bytes_per_row) else {
        return false;
    };
    let Some(source_start) = top_row
        .checked_add(rows)
        .and_then(|row| row.checked_mul(bytes_per_row))
    else {
        return false;
    };
    let Some(source_end) = bottom_row.checked_mul(bytes_per_row) else {
        return false;
    };
    let Some(clear_start) = bottom_row
        .checked_sub(rows)
        .and_then(|row| row.checked_mul(bytes_per_row))
    else {
        return false;
    };

    if destination_start > source_start
        || source_start > source_end
        || clear_start > source_end
        || source_end > shadow.len()
    {
        return false;
    }

    shadow.copy_within(source_start..source_end, destination_start);
    shadow[clear_start..source_end].fill(0);
    true
}

fn verify_shadow_scroll_algorithm() -> bool {
    const BYTES_PER_ROW: usize = 8;
    const TOP_ROW: usize = 1;
    const BOTTOM_ROW: usize = 7;
    const SCROLL_ROWS: usize = 2;

    let mut sample = [0u8; 64];
    let source = (TOP_ROW + SCROLL_ROWS) * BYTES_PER_ROW + 3;
    let destination = TOP_ROW * BYTES_PER_ROW + 3;
    let clear_start = (BOTTOM_ROW - SCROLL_ROWS) * BYTES_PER_ROW;
    let clear_end = BOTTOM_ROW * BYTES_PER_ROW;
    sample[source] = 0x5a;
    sample[clear_end - 1] = 0xa5;

    scroll_shadow_rows(
        &mut sample,
        BYTES_PER_ROW,
        TOP_ROW,
        BOTTOM_ROW,
        SCROLL_ROWS,
    ) && sample[destination] == 0x5a
        && sample[clear_start..clear_end].iter().all(|byte| *byte == 0)
}

unsafe fn volatile_copy(destination: *mut u8, source: *const u8, length: usize) {
    let mut offset = 0usize;

    while offset < length
        && ((destination as usize + offset) & (FLUSH_WORD_BYTES - 1)) != 0
    {
        let value = unsafe { ptr::read(source.add(offset)) };
        unsafe { ptr::write_volatile(destination.add(offset), value) };
        offset += 1;
    }

    while offset + FLUSH_WORD_BYTES <= length {
        let value = unsafe { ptr::read_unaligned(source.add(offset).cast::<u64>()) };
        unsafe { ptr::write_volatile(destination.add(offset).cast::<u64>(), value) };
        offset += FLUSH_WORD_BYTES;
    }

    while offset < length {
        let value = unsafe { ptr::read(source.add(offset)) };
        unsafe { ptr::write_volatile(destination.add(offset), value) };
        offset += 1;
    }
}

fn glyph_advance(character: char) -> usize {
    get_raster(character, FONT_WEIGHT, FONT_HEIGHT)
        .or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT))
        .expect("the fallback glyph must be available")
        .width()
        + LETTER_SPACING
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

#[macro_export]
macro_rules! print {
    ($($argument:tt)*) => {
        $crate::console::_print(format_args!($($argument)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($($argument:tt)*) => {
        $crate::print!("{}\n", format_args!($($argument)*))
    };
}
