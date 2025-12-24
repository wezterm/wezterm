//! Cairo 2D software rendering backend
//!
//! This module provides a pure 2D software rendering path using Cairo,
//! designed for environments without GPU acceleration (VNC, remote desktop, etc.)
//! where the default OpenGL/llvmpipe path is too CPU-intensive.

use ::window::bitmaps::{BitmapImage, Texture2d};
use ::window::Rect;
use anyhow::Context as _;
use cairo::{Format, ImageSurface};
use std::cell::RefCell;

/// A texture implemented as a Cairo ImageSurface
pub struct CairoTexture {
    surface: RefCell<ImageSurface>,
    width: usize,
    height: usize,
}

impl std::fmt::Debug for CairoTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CairoTexture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl CairoTexture {
    pub fn new(width: usize, height: usize) -> anyhow::Result<Self> {
        let surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)
            .context("Failed to create Cairo ImageSurface")?;
        Ok(Self {
            surface: RefCell::new(surface),
            width,
            height,
        })
    }

    /// Get a reference to the underlying Cairo surface
    pub fn surface(&self) -> std::cell::Ref<'_, ImageSurface> {
        self.surface.borrow()
    }

    /// Get a mutable reference to the underlying Cairo surface
    /// (needed for accessing pixel data via Cairo's data() method)
    pub fn surface_mut(&self) -> std::cell::RefMut<'_, ImageSurface> {
        self.surface.borrow_mut()
    }
}

impl Texture2d for CairoTexture {
    fn write(&self, rect: Rect, im: &dyn BitmapImage) {
        let mut surface = self.surface.borrow_mut();
        let (im_width, im_height) = im.image_dimensions();

        // Get raw access to the surface data
        let stride = surface.stride() as usize;
        {
            let mut data = surface.data().expect("Failed to get surface data");

            // Copy pixels from the bitmap image to the Cairo surface
            // Cairo uses ARGB32 (premultiplied), WezTerm uses RGBA32
            // Glyph coverage values are in linear space for GPU rendering,
            // so we apply gamma correction for proper visual weight in Cairo.
            let src_pixels = im.pixels();
            let dest_x = rect.origin.x.max(0) as usize;
            let dest_y = rect.origin.y.max(0) as usize;

            for y in 0..im_height.min(rect.size.height as usize) {
                let src_row_start = y * im_width;
                let dest_row_start = (dest_y + y) * stride / 4;

                for x in 0..im_width.min(rect.size.width as usize) {
                    let src_idx = src_row_start + x;
                    let dest_idx = dest_row_start + dest_x + x;

                    if dest_idx * 4 + 3 < data.len() {
                        let pixel = src_pixels[src_idx];
                        let r = ((pixel >> 0) & 0xFF) as u8;
                        let g = ((pixel >> 8) & 0xFF) as u8;
                        let b = ((pixel >> 16) & 0xFF) as u8;
                        let a = ((pixel >> 24) & 0xFF) as u8;

                        // Cairo expects BGRA in memory (little-endian ARGB32)
                        // with PREMULTIPLIED alpha: RGB values must be multiplied by A
                        let offset = dest_idx * 4;
                        data[offset + 0] = (b as u16 * a as u16 / 255) as u8;
                        data[offset + 1] = (g as u16 * a as u16 / 255) as u8;
                        data[offset + 2] = (r as u16 * a as u16 / 255) as u8;
                        data[offset + 3] = a;
                    }
                }
            }
        }
        surface.mark_dirty();
    }

    fn read(&self, _rect: Rect, _im: &mut dyn BitmapImage) {
        // Not implemented - not needed for rendering
        unimplemented!("CairoTexture::read not implemented");
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }
}
