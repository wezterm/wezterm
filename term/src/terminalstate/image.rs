use crate::{Position, StableRowIndex, TerminalState};
use anyhow::Context;
use humansize::{SizeFormatter, DECIMAL};
use num_traits::{One, Zero};
use ordered_float::NotNan;
use std::sync::Arc;
use wezterm_cell::image::{ImageCell, ImageDataType};
use wezterm_cell::Cell;
use wezterm_surface::change::ImageData;
use wezterm_surface::TextureCoordinate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementInfo {
    pub first_row: StableRowIndex,
    /// Leftmost column of the placement, in viewport coordinates
    pub first_col: usize,
    pub rows: usize,
    pub cols: usize,
    /// The z-index this placement was attached with
    pub z_index: i32,
    /// For relative placements: the (image id, placement id) of the
    /// parent placement
    pub parent: Option<(u32, Option<u32>)>,
    /// For relative placements: the H=/V= cell offsets from the parent
    pub parent_offsets: (i64, i64),
    /// For virtual placements: the (cols, rows) rectangle that unicode
    /// placeholder text may occupy
    pub virtual_rect: Option<(usize, usize)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImageAttachParams {
    /// Dimensions of the underlying ImageData, in pixels
    pub image_width: u32,
    pub image_height: u32,

    /// Dimensions of the area of the image to be displayed, in pixels
    pub source_width: Option<u32>,
    pub source_height: Option<u32>,

    /// Origin of the source data region, top left corner in pixels
    pub source_origin_x: u32,
    pub source_origin_y: u32,

    /// When rendering in the cell, use this offset from the top left
    /// of the cell. This is only used in the Kitty image protocol.
    /// This should be smaller than the size of the cell. Larger values will
    /// be truncated.
    pub cell_padding_left: u16,
    pub cell_padding_top: u16,

    /// Plane on which to display the image
    pub z_index: i32,

    /// Desired number of cells to span.
    /// If None, then compute based on source_width and source_height
    pub columns: Option<usize>,
    pub rows: Option<usize>,

    pub image_id: Option<u32>,
    pub placement_id: Option<u32>,

    pub style: ImageAttachStyle,
    pub do_not_move_cursor: bool,

    /// Explicit placement origin (viewport column, viewport row) for
    /// relative placements; when set, the image attaches there instead
    /// of at the cursor and the cursor never moves
    pub origin: Option<(i64, i64)>,

    pub data: Arc<ImageData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageAttachStyle {
    Sixel,
    Iterm,
    Kitty,
}

impl TerminalState {
    pub(crate) fn assign_image_to_cells(
        &mut self,
        params: ImageAttachParams,
    ) -> anyhow::Result<PlacementInfo> {
        let seqno = self.seqno;
        let physical_cols = self.screen().physical_cols;
        let physical_rows = self.screen().physical_rows;
        let cell_pixel_width = self.pixel_width / physical_cols;
        let cell_pixel_height = self.pixel_height / physical_rows;

        // !!! Guard against missing per-cell pixel dimensions, prevent divide by zero.
        // If pty has no specified pixel size then the per-cell pixel dimensions are zero.
        // A cell-sized image (without explicit `c=`/`r=`) would then divide by zero when computing
        // how many cells it spans.
        // (e.g. the default `PtySize`, as well as for tmux/headless domains)
        // There is no sane way to place a pixel-addressed image without a cell size.
        // => Refuse the image placement instead of panicking and taking down the terminal.
        // <https://github.com/wezterm/wezterm/issues/6344>
        anyhow::ensure!(
            cell_pixel_width != 0 && cell_pixel_height != 0,
            "refusing to display image: terminal has no cell pixel dimensions (WxH: {}x{})",
            cell_pixel_width,
            cell_pixel_height
        );

        let cell_padding_left = params
            .cell_padding_left
            .min(cell_pixel_width.saturating_sub(1) as u16);
        let cell_padding_top = params
            .cell_padding_top
            .min(cell_pixel_height.saturating_sub(1) as u16);
        //NOTE: review conflicting origin vs drawing going over image
        let image_max_width = params.image_width.saturating_sub(params.source_origin_x);
        let image_max_height = params.image_height.saturating_sub(params.source_origin_y);
        let draw_width = params
            .source_width
            .unwrap_or(image_max_width)
            .min(image_max_width);
        let draw_height = params
            .source_height
            .unwrap_or(image_max_height)
            .min(image_max_height);

        // !!! Guard against a zero-sized image, nothing to draw.
        // A zero-sized drawable region leaves nothing to display and would divide by zero when
        // computing the per-cell pixel deltas below.
        // (e.g. an image with explicit `w=0`/`h=0`, or a source origin outside the image bounds)
        // => Refuse the image placement instead of panicking and taking down the terminal.
        // <https://github.com/wezterm/wezterm/issues/6344>
        anyhow::ensure!(
            draw_width != 0 && draw_height != 0,
            "refusing to display image with zero draw dimensions (WxH: {}x{})",
            draw_width,
            draw_height
        );

        // The pixel rectangle that the drawn image area occupies inside
        // the placement grid, relative to the grid's top left corner.
        // With neither c= nor r= the image keeps its natural size.
        // With only one of them the other dimension is derived so that
        // the aspect ratio is preserved. With both, the image is scaled
        // to fit the rectangle and letterboxed (centered), so that it is
        // never distorted.
        let (target_x, target_y, target_w, target_h): (u32, u32, u32, u32) =
            match (params.columns, params.rows) {
                (Some(cols), Some(rows)) => {
                    let box_w = cols as u64 * cell_pixel_width as u64;
                    let box_h = rows as u64 * cell_pixel_height as u64;
                    // Choose the smaller scale factor, comparing the
                    // fractions box_w/draw_width and box_h/draw_height
                    // in integer arithmetic
                    let width_limited = box_w * draw_height as u64 <= draw_width as u64 * box_h;
                    let (w, h) = if width_limited {
                        (
                            box_w,
                            (box_w * draw_height as u64 + draw_width as u64 / 2)
                                / draw_width as u64,
                        )
                    } else {
                        (
                            (box_h * draw_width as u64 + draw_height as u64 / 2)
                                / draw_height as u64,
                            box_h,
                        )
                    };
                    // Rounding may push a pixel outside the box; clamp
                    let w = w.min(box_w) as u32;
                    let h = h.min(box_h) as u32;
                    (
                        ((box_w - w as u64) / 2) as u32,
                        ((box_h - h as u64) / 2) as u32,
                        w,
                        h,
                    )
                }
                (Some(cols), None) => {
                    let w = cols as u64 * cell_pixel_width as u64;
                    let h = (w * draw_height as u64 + draw_width as u64 / 2) / draw_width as u64;
                    (0, 0, w as u32, h as u32)
                }
                (None, Some(rows)) => {
                    let h = rows as u64 * cell_pixel_height as u64;
                    let w = (h * draw_width as u64 + draw_height as u64 / 2) / draw_height as u64;
                    (0, 0, w as u32, h as u32)
                }
                (None, None) => (0, 0, draw_width, draw_height),
            };

        anyhow::ensure!(
            target_w != 0 && target_h != 0,
            "refusing to display image that scales down to zero pixels (WxH: {}x{})",
            target_w,
            target_h
        );

        let width_in_cells = params.columns.unwrap_or_else(|| {
            (target_w as usize + cell_pixel_width as usize - 1) / cell_pixel_width as usize
        });
        let height_in_cells = params.rows.unwrap_or_else(|| {
            (target_h as usize + cell_pixel_height as usize - 1) / cell_pixel_height as usize
        });
        let height_in_cells = if params.do_not_move_cursor {
            height_in_cells.min(self.screen().physical_rows - self.cursor.y as usize)
        } else {
            height_in_cells
        };

        log::debug!(
            "image occupies {}x{} px at offset {},{} inside {}x{} cells (cell is {}x{}), {:?}, (term is {}x{}@{}x{})",
            target_w,
            target_h,
            target_x,
            target_y,
            width_in_cells,
            height_in_cells,
            cell_pixel_width,
            cell_pixel_height,
            params,
            physical_cols,
            physical_rows,
            self.pixel_width,
            self.pixel_height
        );

        let (origin_x, origin_y, at_cursor) = match params.origin {
            Some((ox, oy)) => (ox, oy, false),
            None => (self.cursor.x as i64, self.cursor.y, true),
        };
        // Captured before the loop, which may move the cursor below
        let first_col = if at_cursor {
            self.cursor.x
        } else {
            origin_x.max(0) as usize
        };

        let first_row = self.screen().visible_row_to_stable_row(if at_cursor {
            self.cursor.y
        } else {
            origin_y
        });

        for y in 0..height_in_cells {
            let cell_y0 = y as u32 * cell_pixel_height as u32;
            let cell_y1 = cell_y0 + cell_pixel_height as u32;
            // The X/Y pixel offsets displace the image within the cells
            let usable_y0 = cell_y0 + cell_padding_top as u32;
            let overlap_y0 = usable_y0.max(target_y);
            let overlap_y1 = cell_y1.min(target_y + target_h);

            let cursor_y = if !at_cursor {
                origin_y + y as i64
            } else if params.do_not_move_cursor {
                self.cursor.y + y as i64
            } else {
                self.cursor.y
            };
            log::debug!(
                "setting cells for y={} x=[{}..{}]",
                cursor_y,
                origin_x,
                origin_x + width_in_cells as i64
            );
            for x in 0..width_in_cells {
                let cell_x0 = x as u32 * cell_pixel_width as u32;
                let cell_x1 = cell_x0 + cell_pixel_width as u32;
                let usable_x0 = cell_x0 + cell_padding_left as u32;
                let overlap_x0 = usable_x0.max(target_x);
                let overlap_x1 = cell_x1.min(target_x + target_w);

                // Cells outside of the screen for relative placements
                // are simply not attached
                if !at_cursor
                    && ((origin_x + x as i64) < 0
                        || cursor_y < 0
                        || (origin_x + x as i64) >= physical_cols as i64
                        || cursor_y >= physical_rows as i64)
                {
                    continue;
                }

                // Cells the (letterboxed) image does not reach stay as
                // they are but still count towards the placement grid
                // and the cursor movement below.
                if overlap_x0 >= overlap_x1 || overlap_y0 >= overlap_y1 {
                    continue;
                }

                // Map the overlapping region back into source pixels
                let src_x0 = params.source_origin_x as u64
                    + (overlap_x0 - target_x) as u64 * draw_width as u64 / target_w as u64;
                let src_x1 = params.source_origin_x as u64
                    + (overlap_x1 - target_x) as u64 * draw_width as u64 / target_w as u64;
                let src_y0 = params.source_origin_y as u64
                    + (overlap_y0 - target_y) as u64 * draw_height as u64 / target_h as u64;
                let src_y1 = params.source_origin_y as u64
                    + (overlap_y1 - target_y) as u64 * draw_height as u64 / target_h as u64;

                let top_left = TextureCoordinate::new(
                    NotNan::new(src_x0 as f32 / params.image_width as f32)
                        .context("computing xpos")?,
                    NotNan::new(src_y0 as f32 / params.image_height as f32)
                        .context("computing ypos")?,
                );
                let bottom_right = TextureCoordinate::new(
                    NotNan::new(src_x1 as f32 / params.image_width as f32)
                        .context("computing xpos")?,
                    NotNan::new(src_y1 as f32 / params.image_height as f32)
                        .context("computing ypos")?,
                );

                let cell_x = origin_x + x as i64;
                let mut cell = self
                    .screen_mut()
                    .get_cell(cell_x as usize, cursor_y)
                    .cloned()
                    .unwrap_or_else(Cell::blank);
                let img = Box::new(ImageCell::with_z_index(
                    top_left,
                    bottom_right,
                    params.data.clone(),
                    params.z_index,
                    (overlap_x0 - cell_x0) as u16,
                    (overlap_y0 - cell_y0) as u16,
                    (cell_x1 - overlap_x1) as u16,
                    (cell_y1 - overlap_y1) as u16,
                    params.image_id,
                    params.placement_id,
                ));
                match params.style {
                    ImageAttachStyle::Kitty => cell.attrs_mut().attach_image(img),
                    ImageAttachStyle::Sixel | ImageAttachStyle::Iterm => {
                        cell.attrs_mut().set_image(img)
                    }
                };

                self.screen_mut()
                    .set_cell(cell_x as usize, cursor_y, &cell, seqno);
            }
            if at_cursor && !params.do_not_move_cursor && y < height_in_cells - 1 {
                self.new_line(false);
            }
        }

        // adjust cursor position if the drawn cells move beyond current cell
        let x_padding_shift: i64 = one_or_zero(
            (target_x + target_w) as usize + cell_padding_left as usize
                > cell_pixel_width as usize * width_in_cells,
        );
        let y_padding_shift: i64 = one_or_zero(
            (target_y + target_h) as usize + cell_padding_top as usize
                > cell_pixel_height as usize * height_in_cells,
        );
        if at_cursor && !params.do_not_move_cursor {
            // Sixel places the cursor under the left corner of the image,
            // unless sixel_scrolls_right is enabled.
            // iTerm places it after the bottom right corner.
            let bottom_right = match params.style {
                ImageAttachStyle::Kitty | ImageAttachStyle::Iterm => true,
                ImageAttachStyle::Sixel => self.sixel_scrolls_right,
            };

            if bottom_right {
                self.set_cursor_pos(
                    &Position::Relative(width_in_cells as i64 + x_padding_shift),
                    &Position::Relative(y_padding_shift),
                );
            }
        }

        Ok(PlacementInfo {
            first_row,
            first_col,
            rows: height_in_cells,
            cols: width_in_cells,
            z_index: params.z_index,
            parent: None,
            parent_offsets: (0, 0),
            virtual_rect: None,
        })
    }

    /// cache recent images and avoid assigning a new id for repeated data!
    pub(crate) fn raw_image_to_image_data(
        &mut self,
        data: ImageDataType,
    ) -> Result<Arc<ImageData>, termwiz::error::InternalError> {
        let key = data.compute_hash();
        if let Some(item) = self.image_cache.get(&key) {
            Ok(Arc::clone(item))
        } else {
            let data = data.swap_out()?;
            let image_data = Arc::new(ImageData::with_data(data));
            self.image_cache.put(key, Arc::clone(&image_data));
            Ok(image_data)
        }
    }
}

/// An error carrying one of the kitty graphics protocol error codes
/// (ENOENT, EINVAL, ENOSPC, EBADF, ...), so that the terminal can
/// answer the client with a spec-compliant response.
#[derive(Debug)]
pub(crate) struct KittyProtocolError {
    pub code: &'static str,
    pub detail: String,
}

impl std::fmt::Display for KittyProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}", self.code, self.detail)
    }
}

impl std::error::Error for KittyProtocolError {}

pub(crate) fn check_image_dimensions(width: u32, height: u32) -> anyhow::Result<()> {
    const MAX_IMAGE_SIZE: u32 = 100_000_000;
    let size = width.saturating_mul(height).saturating_mul(4);
    if size > MAX_IMAGE_SIZE {
        return Err(KittyProtocolError {
            code: "ENOSPC",
            detail: format!(
                "image of {}x{} needs {} RAM which is more than the maximum {}",
                width,
                height,
                SizeFormatter::new(size, DECIMAL),
                SizeFormatter::new(MAX_IMAGE_SIZE, DECIMAL)
            ),
        }
        .into());
    }
    if size == 0 {
        return Err(KittyProtocolError {
            code: "EINVAL",
            detail: "image with 0x0 dimensions".to_string(),
        }
        .into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: image::ImageFormat,
}

pub(crate) fn dimensions(data: &[u8]) -> anyhow::Result<ImageInfo> {
    let reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow::anyhow!("unknown format!?"))?;
    let (width, height) = reader.into_dimensions()?;
    Ok(ImageInfo {
        width,
        height,
        format,
    })
}

/// Returns `1` if `b` is true, else `0`,
fn one_or_zero<T: Zero + One>(b: bool) -> T {
    if b {
        T::one()
    } else {
        T::zero()
    }
}
