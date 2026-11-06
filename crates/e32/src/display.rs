// SPDX-License-Identifier: Apache-2.0
//! The frame buffer, written directly — what the screen driver (scdv) does when there is no window server.

use crate::sys::{self, ui};

/// The current display mode's frame buffer (32 bits per pixel, `0x00RRGGBB`).
#[derive(Debug)]
pub struct Display {
    pub width: usize,
    pub height: usize,
    /// Bytes between the starts of two lines.
    pub stride: usize,
    base: *mut u8,
}

impl Display {
    /// The current mode from the HAL. `Err(KErrNotSupported)` unless it is 32 bits per pixel.
    pub fn current() -> Result<Self, i32> {
        let i = ui::display_mode_info()?;
        if i.i_bits_per_pixel != 32 || i.i_video_address == 0 {
            return Err(sys::err::KERR_NOT_SUPPORTED);
        }
        Ok(Self {
            width: i.i_size_in_pixels[0] as usize,
            height: i.i_size_in_pixels[1] as usize,
            stride: i.i_offset_between_lines as usize,
            base: (i.i_video_address as usize + i.i_offset_to_first_pixel as usize) as *mut u8,
        })
    }

    /// Copy the rectangle `(x, y, w, h)` of `src` (a `width`-sized frame, `src_stride` pixels per line) to the
    /// screen; clipped to both.
    pub fn blit(&self, src: &[u32], src_stride: usize, x: usize, y: usize, w: usize, h: usize) {
        let w = w
            .min(self.width.saturating_sub(x))
            .min(src_stride.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        for row in y..y + h {
            let Some(line) = src.get(row * src_stride + x..row * src_stride + x + w) else {
                return;
            };
            // SAFETY: the row is inside the mode's frame buffer (clipped above); it is mapped user-writable
            // (EMapAttrUserRw, uncached/buffered, so no cache maintenance is needed).
            unsafe {
                let dst = self.base.add(row * self.stride + x * 4).cast::<u32>();
                core::ptr::copy_nonoverlapping(line.as_ptr(), dst, w);
            }
        }
    }
}
