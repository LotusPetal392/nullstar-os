//! Concrete software surface and authenticated input routing for trusted portal pickers.

use core::{array, convert::Infallible};

use crate::application_portal_gui::{
    MIN_PICKER_HEIGHT, MIN_PICKER_WIDTH, PICKER_FOOTER_HEIGHT, PICKER_HEADER_HEIGHT,
    PICKER_ROW_HEIGHT, PortalColor, PortalPickerEvent, PortalPickerRenderTarget, PortalRect,
};

const GLYPH_WIDTH: u32 = 5;
const GLYPH_HEIGHT: u32 = 7;
const GLYPH_SCALE: u32 = 2;
const GLYPH_ADVANCE: u32 = (GLYPH_WIDTH + 1) * GLYPH_SCALE;
const MAX_PORTAL_INPUT_BINDINGS: usize = 16;
const LIST_HORIZONTAL_INSET: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalSurfaceCreateError {
    ZeroDimensions,
    InvalidStride,
    BufferTooSmall,
    SizeOverflow,
}

/// Mutable ARGB8888 software surface backed by compositor-owned memory.
pub struct PortalArgbSurface<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
    stride: u32,
}

impl<'a> PortalArgbSurface<'a> {
    pub fn new(
        pixels: &'a mut [u32],
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<Self, PortalSurfaceCreateError> {
        if width == 0 || height == 0 {
            return Err(PortalSurfaceCreateError::ZeroDimensions);
        }
        if stride < width {
            return Err(PortalSurfaceCreateError::InvalidStride);
        }
        let required = usize::try_from(height - 1)
            .ok()
            .and_then(|rows| rows.checked_mul(stride as usize))
            .and_then(|prefix| prefix.checked_add(width as usize))
            .ok_or(PortalSurfaceCreateError::SizeOverflow)?;
        if pixels.len() < required {
            return Err(PortalSurfaceCreateError::BufferTooSmall);
        }
        Ok(Self {
            pixels,
            width,
            height,
            stride,
        })
    }

    pub fn pixels(&self) -> &[u32] {
        self.pixels
    }

    fn paint_rect(&mut self, rect: PortalRect, color: PortalColor) {
        let x_end = rect.x.saturating_add(rect.width).min(self.width);
        let y_end = rect.y.saturating_add(rect.height).min(self.height);
        for y in rect.y.min(self.height)..y_end {
            let row = y as usize * self.stride as usize;
            for x in rect.x.min(self.width)..x_end {
                self.pixels[row + x as usize] = color.0;
            }
        }
    }

    fn paint_glyph(
        &mut self,
        x: u32,
        top: u32,
        glyph: [u8; GLYPH_HEIGHT as usize],
        color: PortalColor,
    ) {
        for (row, bits) in glyph.into_iter().enumerate() {
            for column in 0..GLYPH_WIDTH {
                if bits & (1 << (GLYPH_WIDTH - 1 - column)) == 0 {
                    continue;
                }
                self.paint_rect(
                    PortalRect {
                        x: x.saturating_add(column * GLYPH_SCALE),
                        y: top.saturating_add(row as u32 * GLYPH_SCALE),
                        width: GLYPH_SCALE,
                        height: GLYPH_SCALE,
                    },
                    color,
                );
            }
        }
    }
}

impl PortalPickerRenderTarget for PortalArgbSurface<'_> {
    type Error = Infallible;

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn fill_rect(&mut self, rect: PortalRect, color: PortalColor) -> Result<(), Self::Error> {
        self.paint_rect(rect, color);
        Ok(())
    }

    fn stroke_rect(&mut self, rect: PortalRect, color: PortalColor) -> Result<(), Self::Error> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        self.paint_rect(PortalRect { height: 1, ..rect }, color);
        self.paint_rect(
            PortalRect {
                y: rect.y.saturating_add(rect.height - 1),
                height: 1,
                ..rect
            },
            color,
        );
        self.paint_rect(PortalRect { width: 1, ..rect }, color);
        self.paint_rect(
            PortalRect {
                x: rect.x.saturating_add(rect.width - 1),
                width: 1,
                ..rect
            },
            color,
        );
        Ok(())
    }

    fn draw_text(
        &mut self,
        x: u32,
        baseline_y: u32,
        text: &[u8],
        color: PortalColor,
    ) -> Result<(), Self::Error> {
        let top = baseline_y.saturating_sub(GLYPH_HEIGHT * GLYPH_SCALE);
        let mut cursor = x;
        for byte in text.iter().copied() {
            if cursor >= self.width {
                break;
            }
            self.paint_glyph(cursor, top, glyph_rows(byte), color);
            cursor = cursor.saturating_add(GLYPH_ADVANCE);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Confirm,
    Back,
    Escape,
    LoadMore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalButton {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalButtonState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalRawInput {
    Key {
        key: PortalKey,
        state: PortalButtonState,
    },
    PointerButton {
        x: u32,
        y: u32,
        button: PortalButton,
        state: PortalButtonState,
        click_count: u8,
    },
    ScrollRows(i8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalInputEnvelope {
    sender_process_id: u64,
    session_id: u64,
    surface_id: u64,
    sequence: u64,
    input: PortalRawInput,
}

impl PortalInputEnvelope {
    pub const fn new(
        sender_process_id: u64,
        session_id: u64,
        surface_id: u64,
        sequence: u64,
        input: PortalRawInput,
    ) -> Option<Self> {
        if sender_process_id == 0 || session_id == 0 || surface_id == 0 || sequence == 0 {
            return None;
        }
        Some(Self {
            sender_process_id,
            session_id,
            surface_id,
            sequence,
            input,
        })
    }

    pub const fn session_id(self) -> u64 {
        self.session_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PortalInputBinding {
    session_id: u64,
    surface_id: u64,
    width: u32,
    height: u32,
    last_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalInputBindingError {
    InvalidIdentity,
    SurfaceTooSmall,
    SessionAlreadyBound,
    SurfaceAlreadyBound,
    Capacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalInputError {
    UnauthorizedSender,
    UnknownSurface,
    WrongSession,
    Replay,
    InvalidClickCount,
}

/// Authenticates compositor input and binds it to one secure picker surface.
pub struct PortalPickerInputRouter {
    trusted_compositor_process_id: u64,
    bindings: [Option<PortalInputBinding>; MAX_PORTAL_INPUT_BINDINGS],
}

impl PortalPickerInputRouter {
    pub fn new(trusted_compositor_process_id: u64) -> Option<Self> {
        if trusted_compositor_process_id == 0 {
            return None;
        }
        Some(Self {
            trusted_compositor_process_id,
            bindings: array::from_fn(|_| None),
        })
    }

    pub fn bind(
        &mut self,
        session_id: u64,
        surface_id: u64,
        width: u32,
        height: u32,
    ) -> Result<(), PortalInputBindingError> {
        if session_id == 0 || surface_id == 0 {
            return Err(PortalInputBindingError::InvalidIdentity);
        }
        if width < MIN_PICKER_WIDTH || height < MIN_PICKER_HEIGHT {
            return Err(PortalInputBindingError::SurfaceTooSmall);
        }
        if self
            .bindings
            .iter()
            .flatten()
            .any(|binding| binding.session_id == session_id)
        {
            return Err(PortalInputBindingError::SessionAlreadyBound);
        }
        if self
            .bindings
            .iter()
            .flatten()
            .any(|binding| binding.surface_id == surface_id)
        {
            return Err(PortalInputBindingError::SurfaceAlreadyBound);
        }
        let slot = self
            .bindings
            .iter_mut()
            .find(|binding| binding.is_none())
            .ok_or(PortalInputBindingError::Capacity)?;
        *slot = Some(PortalInputBinding {
            session_id,
            surface_id,
            width,
            height,
            last_sequence: 0,
        });
        Ok(())
    }

    pub fn unbind_session(&mut self, session_id: u64) -> bool {
        let Some(slot) = self.bindings.iter_mut().find(|binding| {
            binding
                .as_ref()
                .is_some_and(|binding| binding.session_id == session_id)
        }) else {
            return false;
        };
        *slot = None;
        true
    }

    pub fn translate(
        &mut self,
        envelope: PortalInputEnvelope,
        first_visible: usize,
        entry_count: usize,
    ) -> Result<Option<PortalPickerEvent>, PortalInputError> {
        if envelope.sender_process_id != self.trusted_compositor_process_id {
            return Err(PortalInputError::UnauthorizedSender);
        }
        let binding = self
            .bindings
            .iter_mut()
            .flatten()
            .find(|binding| binding.surface_id == envelope.surface_id)
            .ok_or(PortalInputError::UnknownSurface)?;
        if binding.session_id != envelope.session_id {
            return Err(PortalInputError::WrongSession);
        }
        if envelope.sequence <= binding.last_sequence {
            return Err(PortalInputError::Replay);
        }
        binding.last_sequence = envelope.sequence;

        match envelope.input {
            PortalRawInput::Key {
                state: PortalButtonState::Released,
                ..
            }
            | PortalRawInput::PointerButton {
                state: PortalButtonState::Released,
                ..
            } => Ok(None),
            PortalRawInput::Key { key, .. } => Ok(Some(key_event(key))),
            PortalRawInput::ScrollRows(rows) => Ok(match rows.cmp(&0) {
                core::cmp::Ordering::Less => Some(PortalPickerEvent::MoveDown),
                core::cmp::Ordering::Greater => Some(PortalPickerEvent::MoveUp),
                core::cmp::Ordering::Equal => None,
            }),
            PortalRawInput::PointerButton {
                x,
                y,
                button: PortalButton::Secondary,
                ..
            } => {
                let _ = (x, y);
                Ok(None)
            }
            PortalRawInput::PointerButton {
                x,
                y,
                button: PortalButton::Primary,
                click_count,
                ..
            } => {
                if !matches!(click_count, 1 | 2) {
                    return Err(PortalInputError::InvalidClickCount);
                }
                let right = binding.width.saturating_sub(LIST_HORIZONTAL_INSET);
                let bottom = binding.height.saturating_sub(PICKER_FOOTER_HEIGHT);
                if x < LIST_HORIZONTAL_INSET
                    || x >= right
                    || y < PICKER_HEADER_HEIGHT
                    || y >= bottom
                {
                    return Ok(None);
                }
                let row = ((y - PICKER_HEADER_HEIGHT) / PICKER_ROW_HEIGHT) as usize;
                let visible_rows = ((bottom - PICKER_HEADER_HEIGHT) / PICKER_ROW_HEIGHT) as usize;
                if row >= visible_rows {
                    return Ok(None);
                }
                let Some(index) = first_visible.checked_add(row) else {
                    return Ok(None);
                };
                if index >= entry_count {
                    return Ok(None);
                }
                Ok(Some(if click_count == 2 {
                    PortalPickerEvent::ActivateIndex(index)
                } else {
                    PortalPickerEvent::SelectIndex(index)
                }))
            }
        }
    }
}

const fn key_event(key: PortalKey) -> PortalPickerEvent {
    match key {
        PortalKey::Up => PortalPickerEvent::MoveUp,
        PortalKey::Down => PortalPickerEvent::MoveDown,
        PortalKey::PageUp => PortalPickerEvent::PageUp,
        PortalKey::PageDown => PortalPickerEvent::PageDown,
        PortalKey::Enter => PortalPickerEvent::Activate,
        PortalKey::Confirm => PortalPickerEvent::Confirm,
        PortalKey::Back => PortalPickerEvent::NavigateBack,
        PortalKey::Escape => PortalPickerEvent::Cancel,
        PortalKey::LoadMore => PortalPickerEvent::RequestNextPage,
    }
}

const fn glyph_rows(byte: u8) -> [u8; GLYPH_HEIGHT as usize] {
    match byte.to_ascii_uppercase() {
        b' ' => [0, 0, 0, 0, 0, 0, 0],
        b'A' => [14, 17, 17, 31, 17, 17, 17],
        b'B' => [30, 17, 17, 30, 17, 17, 30],
        b'C' => [14, 17, 16, 16, 16, 17, 14],
        b'D' => [30, 17, 17, 17, 17, 17, 30],
        b'E' => [31, 16, 16, 30, 16, 16, 31],
        b'F' => [31, 16, 16, 30, 16, 16, 16],
        b'G' => [14, 17, 16, 23, 17, 17, 14],
        b'H' => [17, 17, 17, 31, 17, 17, 17],
        b'I' => [14, 4, 4, 4, 4, 4, 14],
        b'J' => [7, 2, 2, 2, 18, 18, 12],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        b'L' => [16, 16, 16, 16, 16, 16, 31],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'N' => [17, 25, 21, 19, 17, 17, 17],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'Q' => [14, 17, 17, 17, 21, 18, 13],
        b'R' => [30, 17, 17, 30, 20, 18, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'U' => [17, 17, 17, 17, 17, 17, 14],
        b'V' => [17, 17, 17, 17, 17, 10, 4],
        b'W' => [17, 17, 17, 21, 21, 21, 10],
        b'X' => [17, 17, 10, 4, 10, 17, 17],
        b'Y' => [17, 17, 10, 4, 4, 4, 4],
        b'Z' => [31, 1, 2, 4, 8, 16, 31],
        b'0' => [14, 17, 19, 21, 25, 17, 14],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        b'8' => [14, 17, 17, 14, 17, 17, 14],
        b'9' => [14, 17, 17, 15, 1, 1, 14],
        b'.' => [0, 0, 0, 0, 0, 12, 12],
        b',' => [0, 0, 0, 0, 4, 4, 8],
        b':' => [0, 12, 12, 0, 12, 12, 0],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'_' => [0, 0, 0, 0, 0, 0, 31],
        b'/' => [1, 2, 2, 4, 8, 8, 16],
        b'[' => [14, 8, 8, 8, 8, 8, 14],
        b']' => [14, 2, 2, 2, 2, 2, 14],
        _ => [31, 17, 1, 2, 4, 0, 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_validates_stride_and_buffer_bounds() {
        let mut pixels = [0_u32; 12];
        assert!(matches!(
            PortalArgbSurface::new(&mut pixels, 4, 3, 3),
            Err(PortalSurfaceCreateError::InvalidStride)
        ));
        assert!(matches!(
            PortalArgbSurface::new(&mut pixels[..11], 4, 3, 4),
            Err(PortalSurfaceCreateError::BufferTooSmall)
        ));
        assert!(PortalArgbSurface::new(&mut pixels, 4, 3, 4).is_ok());
    }

    #[test]
    fn software_surface_clips_and_rasterizes_text() {
        let mut pixels = [0_u32; 20 * 20];
        let mut surface = PortalArgbSurface::new(&mut pixels, 20, 20, 20).unwrap();
        surface.draw_text(1, 16, b"A", PortalColor::TEXT).unwrap();
        surface
            .fill_rect(
                PortalRect {
                    x: 19,
                    y: 19,
                    width: u32::MAX,
                    height: u32::MAX,
                },
                PortalColor::SELECTED,
            )
            .unwrap();
        assert!(
            surface
                .pixels()
                .iter()
                .any(|pixel| *pixel == PortalColor::TEXT.0)
        );
        assert_eq!(surface.pixels()[19 * 20 + 19], PortalColor::SELECTED.0);
    }

    fn key_envelope(sender: u64, sequence: u64) -> PortalInputEnvelope {
        PortalInputEnvelope::new(
            sender,
            7,
            9,
            sequence,
            PortalRawInput::Key {
                key: PortalKey::Down,
                state: PortalButtonState::Pressed,
            },
        )
        .unwrap()
    }

    #[test]
    fn input_router_authenticates_binding_and_rejects_replay() {
        let mut router = PortalPickerInputRouter::new(42).unwrap();
        router.bind(7, 9, 320, 200).unwrap();
        assert_eq!(
            router.translate(key_envelope(41, 1), 0, 3),
            Err(PortalInputError::UnauthorizedSender)
        );
        assert_eq!(
            router.translate(key_envelope(42, 1), 0, 3),
            Ok(Some(PortalPickerEvent::MoveDown))
        );
        assert_eq!(
            router.translate(key_envelope(42, 1), 0, 3),
            Err(PortalInputError::Replay)
        );
    }

    #[test]
    fn pointer_input_maps_only_visible_authenticated_rows() {
        let mut router = PortalPickerInputRouter::new(42).unwrap();
        router.bind(7, 9, 320, 200).unwrap();
        let click = |sequence, y, click_count| {
            PortalInputEnvelope::new(
                42,
                7,
                9,
                sequence,
                PortalRawInput::PointerButton {
                    x: 20,
                    y,
                    button: PortalButton::Primary,
                    state: PortalButtonState::Pressed,
                    click_count,
                },
            )
            .unwrap()
        };
        assert_eq!(
            router.translate(click(1, PICKER_HEADER_HEIGHT + PICKER_ROW_HEIGHT, 1), 2, 6),
            Ok(Some(PortalPickerEvent::SelectIndex(3)))
        );
        assert_eq!(
            router.translate(click(2, PICKER_HEADER_HEIGHT, 2), 2, 6),
            Ok(Some(PortalPickerEvent::ActivateIndex(2)))
        );
        assert_eq!(router.translate(click(3, 20, 1), 2, 6), Ok(None));
    }
}
