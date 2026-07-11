use crate::terminalstate::image::*;
use crate::terminalstate::{ImageAttachParams, PlacementInfo};
use crate::{StableRowIndex, TerminalState, VisibleRowIndex};
use ::image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, RgbImage, Rgba, RgbaImage,
};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use wezterm_cell::color::ColorAttribute;
use wezterm_cell::image::{ImageCell, ImageDataType, TextureCoordinate};
use wezterm_cell::{Cell, CellAttributes};
use wezterm_escape_parser::apc::{
    KittyFrameCompositionMode, KittyImage, KittyImageCompression, KittyImageData, KittyImageDelete,
    KittyImageFormat, KittyImageFrame, KittyImageFrameCompose, KittyImagePlacement,
    KittyImageTransmit, KittyImageVerbosity,
};
use wezterm_surface::change::ImageData;
use wezterm_surface::SequenceNo;

/// The placeholder character used to reference virtual placements.
/// <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>
pub(crate) const KITTY_UNICODE_PLACEHOLDER: char = '\u{10EEEE}';

/// The combining characters used to encode row, column and
/// most-significant-image-id-byte values on kitty image placeholder cells.
/// The value encoded by a combining character is its index in this table,
/// which mirrors rowcolumn-diacritics.txt from the kitty source and must
/// remain sorted so that we can binary search it.
#[rustfmt::skip]
const KITTY_ROW_COLUMN_DIACRITICS: &[u32] = &[
    0x00305, 0x0030d, 0x0030e, 0x00310, 0x00312, 0x0033d, 0x0033e, 0x0033f, 0x00346, 0x0034a, 0x0034b, 0x0034c,
    0x00350, 0x00351, 0x00352, 0x00357, 0x0035b, 0x00363, 0x00364, 0x00365, 0x00366, 0x00367, 0x00368, 0x00369,
    0x0036a, 0x0036b, 0x0036c, 0x0036d, 0x0036e, 0x0036f, 0x00483, 0x00484, 0x00485, 0x00486, 0x00487, 0x00592,
    0x00593, 0x00594, 0x00595, 0x00597, 0x00598, 0x00599, 0x0059c, 0x0059d, 0x0059e, 0x0059f, 0x005a0, 0x005a1,
    0x005a8, 0x005a9, 0x005ab, 0x005ac, 0x005af, 0x005c4, 0x00610, 0x00611, 0x00612, 0x00613, 0x00614, 0x00615,
    0x00616, 0x00617, 0x00657, 0x00658, 0x00659, 0x0065a, 0x0065b, 0x0065d, 0x0065e, 0x006d6, 0x006d7, 0x006d8,
    0x006d9, 0x006da, 0x006db, 0x006dc, 0x006df, 0x006e0, 0x006e1, 0x006e2, 0x006e4, 0x006e7, 0x006e8, 0x006eb,
    0x006ec, 0x00730, 0x00732, 0x00733, 0x00735, 0x00736, 0x0073a, 0x0073d, 0x0073f, 0x00740, 0x00741, 0x00743,
    0x00745, 0x00747, 0x00749, 0x0074a, 0x007eb, 0x007ec, 0x007ed, 0x007ee, 0x007ef, 0x007f0, 0x007f1, 0x007f3,
    0x00816, 0x00817, 0x00818, 0x00819, 0x0081b, 0x0081c, 0x0081d, 0x0081e, 0x0081f, 0x00820, 0x00821, 0x00822,
    0x00823, 0x00825, 0x00826, 0x00827, 0x00829, 0x0082a, 0x0082b, 0x0082c, 0x0082d, 0x00951, 0x00953, 0x00954,
    0x00f82, 0x00f83, 0x00f86, 0x00f87, 0x0135d, 0x0135e, 0x0135f, 0x017dd, 0x0193a, 0x01a17, 0x01a75, 0x01a76,
    0x01a77, 0x01a78, 0x01a79, 0x01a7a, 0x01a7b, 0x01a7c, 0x01b6b, 0x01b6d, 0x01b6e, 0x01b6f, 0x01b70, 0x01b71,
    0x01b72, 0x01b73, 0x01cd0, 0x01cd1, 0x01cd2, 0x01cda, 0x01cdb, 0x01ce0, 0x01dc0, 0x01dc1, 0x01dc3, 0x01dc4,
    0x01dc5, 0x01dc6, 0x01dc7, 0x01dc8, 0x01dc9, 0x01dcb, 0x01dcc, 0x01dd1, 0x01dd2, 0x01dd3, 0x01dd4, 0x01dd5,
    0x01dd6, 0x01dd7, 0x01dd8, 0x01dd9, 0x01dda, 0x01ddb, 0x01ddc, 0x01ddd, 0x01dde, 0x01ddf, 0x01de0, 0x01de1,
    0x01de2, 0x01de3, 0x01de4, 0x01de5, 0x01de6, 0x01dfe, 0x020d0, 0x020d1, 0x020d4, 0x020d5, 0x020d6, 0x020d7,
    0x020db, 0x020dc, 0x020e1, 0x020e7, 0x020e9, 0x020f0, 0x02cef, 0x02cf0, 0x02cf1, 0x02de0, 0x02de1, 0x02de2,
    0x02de3, 0x02de4, 0x02de5, 0x02de6, 0x02de7, 0x02de8, 0x02de9, 0x02dea, 0x02deb, 0x02dec, 0x02ded, 0x02dee,
    0x02def, 0x02df0, 0x02df1, 0x02df2, 0x02df3, 0x02df4, 0x02df5, 0x02df6, 0x02df7, 0x02df8, 0x02df9, 0x02dfa,
    0x02dfb, 0x02dfc, 0x02dfd, 0x02dfe, 0x02dff, 0x0a66f, 0x0a67c, 0x0a67d, 0x0a6f0, 0x0a6f1, 0x0a8e0, 0x0a8e1,
    0x0a8e2, 0x0a8e3, 0x0a8e4, 0x0a8e5, 0x0a8e6, 0x0a8e7, 0x0a8e8, 0x0a8e9, 0x0a8ea, 0x0a8eb, 0x0a8ec, 0x0a8ed,
    0x0a8ee, 0x0a8ef, 0x0a8f0, 0x0a8f1, 0x0aab0, 0x0aab2, 0x0aab3, 0x0aab7, 0x0aab8, 0x0aabe, 0x0aabf, 0x0aac1,
    0x0fe20, 0x0fe21, 0x0fe22, 0x0fe23, 0x0fe24, 0x0fe25, 0x0fe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186, 0x1d187,
    0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];

/// Returns the row/column/id value encoded by a placeholder diacritic
fn diacritic_value(c: char) -> Option<u32> {
    KITTY_ROW_COLUMN_DIACRITICS
        .binary_search(&(c as u32))
        .ok()
        .map(|idx| idx as u32)
}

/// Decodes the 24-bit id that placeholder cells smuggle through a color
/// attribute: a true color carries it in the rgb bits, while a palette
/// index carries the low 8 bits.
fn color_encoded_id(color: ColorAttribute) -> Option<u32> {
    match color {
        ColorAttribute::TrueColorWithPaletteFallback(c, _)
        | ColorAttribute::TrueColorWithDefaultFallback(c) => {
            let (r, g, b, _) = c.to_srgb_u8();
            Some(((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
        }
        ColorAttribute::PaletteIndex(idx) => Some(idx as u32),
        ColorAttribute::Default => None,
    }
}

/// A kitty virtual placement (`U=1`): remembers how the image is to be
/// mapped onto a grid of cells. The image is only displayed where the
/// application prints placeholder cells referencing it.
#[derive(Debug, Clone, Copy)]
struct KittyVirtualPlacement {
    cols: usize,
    rows: usize,
    z_index: i32,
}

#[derive(Debug, Default)]
pub struct KittyImageState {
    accumulator: Vec<KittyImage>,
    max_image_id: u32,
    number_to_id: HashMap<u32, u32>,
    id_to_data: HashMap<u32, Arc<ImageData>>,
    placements: HashMap<(u32, Option<u32>), PlacementInfo>,
    virtual_placements: HashMap<(u32, Option<u32>), KittyVirtualPlacement>,
    used_memory: usize,
}

impl KittyImageState {
    fn remove_data_for_id(&mut self, image_id: u32) {
        if let Some(data) = self.id_to_data.remove(&image_id) {
            self.used_memory = self.used_memory.saturating_sub(data.len());
        }
    }

    fn record_id_to_data(&mut self, image_id: u32, data: Arc<ImageData>) {
        if image_id != 0 {
            self.remove_data_for_id(image_id);
        }
        self.prune_unreferenced();
        self.used_memory += data.len();
        self.id_to_data.insert(image_id, data);
    }

    fn prune_unreferenced(&mut self) {
        let budget = 320 * 1024 * 1024; // FIXME: make this configurable
        if self.used_memory > budget {
            let referenced: HashSet<u32> = self
                .placements
                .keys()
                .chain(self.virtual_placements.keys())
                .map(|(k, _)| *k)
                .collect();
            let target = self.used_memory - budget;
            let mut freed = 0;
            self.id_to_data.retain(|id, data| {
                if referenced.contains(id) || freed > target {
                    true
                } else {
                    freed += data.len();
                    false
                }
            });

            log::info!(
                "using {} RAM for images, pruned {}",
                self.used_memory,
                freed
            );
            self.used_memory = self.used_memory.saturating_sub(freed);
        }
    }
}

impl TerminalState {
    fn kitty_img_place(
        &mut self,
        image_id: Option<u32>,
        image_number: Option<u32>,
        placement: KittyImagePlacement,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let image_id = match image_id {
            Some(id) => id,
            None => *self
                .kitty_img
                .number_to_id
                .get(
                    &image_number
                        .ok_or_else(|| anyhow::anyhow!("no image_id or image_number specified!"))?,
                )
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "image_number has no matching image id {:?} in number_to_id",
                        image_number
                    )
                })?,
        };

        log::trace!(
            "kitty_img_place image_id {:?} image_no {:?} placement {:?} verb {:?}",
            image_id,
            image_number,
            placement,
            verbosity
        );
        if image_id != 0 && !placement.unicode_placeholder {
            self.kitty_remove_placement(image_id, placement.placement_id);
        }
        let img = Arc::clone(self.kitty_img.id_to_data.get(&image_id).ok_or_else(|| {
            anyhow::anyhow!(
                "no matching image id {} in id_to_data for image_number {:?}",
                image_id,
                image_number
            )
        })?);

        let (image_width, image_height) = img.data().dimensions()?;

        if placement.unicode_placeholder {
            return self.kitty_img_place_virtual(image_id, placement, image_width, image_height);
        }

        let info = self.assign_image_to_cells(ImageAttachParams {
            image_width,
            image_height,
            source_width: placement.w,
            source_height: placement.h,
            source_origin_x: placement.x.unwrap_or(0),
            source_origin_y: placement.y.unwrap_or(0),
            cell_padding_left: placement.x_offset.unwrap_or(0) as u16,
            cell_padding_top: placement.y_offset.unwrap_or(0) as u16,
            data: img,
            style: ImageAttachStyle::Kitty,
            z_index: placement.z_index.unwrap_or(0),
            columns: placement.columns.map(|x| x as usize),
            rows: placement.rows.map(|x| x as usize),
            image_id: Some(image_id),
            placement_id: placement.placement_id,
            do_not_move_cursor: placement.do_not_move_cursor,
        })?;

        self.kitty_img
            .placements
            .insert((image_id, placement.placement_id), info);
        log::trace!(
            "record placement for {} (image_number {:?}) {:?}",
            image_id,
            image_number,
            placement.placement_id
        );

        Ok(())
    }

    /// Record a virtual placement (`U=1`). It doesn't touch the cell model
    /// or the cursor; the image shows up wherever the application later
    /// prints U+10EEEE placeholder cells that reference it.
    fn kitty_img_place_virtual(
        &mut self,
        image_id: u32,
        placement: KittyImagePlacement,
        image_width: u32,
        image_height: u32,
    ) -> anyhow::Result<()> {
        let (cols, rows) = match (placement.columns, placement.rows) {
            (Some(c), Some(r)) => (c as usize, r as usize),
            (c, r) => {
                // The client left the grid size up to us; size it the same
                // way a regular placement would be, from the cell geometry.
                let cell_pixel_width = self.pixel_width / self.screen().physical_cols;
                let cell_pixel_height = self.pixel_height / self.screen().physical_rows;
                anyhow::ensure!(
                    cell_pixel_width != 0 && cell_pixel_height != 0,
                    "virtual placement has no explicit grid and the \
                     terminal has no cell pixel dimensions"
                );
                (
                    c.map(|c| c as usize)
                        .unwrap_or_else(|| (image_width as usize).div_ceil(cell_pixel_width)),
                    r.map(|r| r as usize)
                        .unwrap_or_else(|| (image_height as usize).div_ceil(cell_pixel_height)),
                )
            }
        };
        anyhow::ensure!(
            cols > 0 && rows > 0,
            "refusing virtual placement with zero dimensions ({}x{})",
            cols,
            rows
        );

        self.kitty_img.virtual_placements.insert(
            (image_id, placement.placement_id),
            KittyVirtualPlacement {
                cols,
                rows,
                z_index: placement.z_index.unwrap_or(0),
            },
        );
        log::trace!(
            "record virtual placement for {} {:?}: {}x{} cells",
            image_id,
            placement.placement_id,
            cols,
            rows
        );
        Ok(())
    }

    /// Print a kitty image placeholder cell: decode which tile of which
    /// virtual placement it references and attach that slice of the image
    /// to the cell in place of the placeholder text.
    /// Returns false if the grapheme doesn't resolve to a virtual
    /// placement; the caller should then print it as regular text.
    /// <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>
    pub(crate) fn kitty_print_unicode_placeholder(
        &mut self,
        x: usize,
        y: VisibleRowIndex,
        g: &str,
        pen: &CellAttributes,
        seqno: SequenceNo,
    ) -> bool {
        if !self.config.enable_kitty_graphics() {
            return false;
        }

        // The placeholder carries up to three diacritics:
        // row, column, and the most significant byte of the image id.
        let mut values = [None; 3];
        let mut chars = g.chars();
        chars.next(); // the placeholder char itself
        for (i, c) in chars.enumerate() {
            match (i < values.len(), diacritic_value(c)) {
                (true, Some(v)) => values[i] = Some(v),
                // Anything else combined with the placeholder isn't a
                // well formed reference
                _ => return false,
            }
        }

        // The low 24 bits of the image id travel in the foreground color,
        // the optional high byte in the third diacritic. An optional
        // placement id travels in the underline color.
        let mut image_id = match color_encoded_id(pen.foreground()) {
            Some(id) => id,
            None => return false,
        };
        if let Some(msb) = values[2] {
            image_id |= msb << 24;
        }
        let placement_id = color_encoded_id(pen.underline_color());

        let vp = match self
            .kitty_img
            .virtual_placements
            .get(&(image_id, placement_id))
            .or_else(|| self.kitty_img.virtual_placements.get(&(image_id, None)))
        {
            Some(vp) => *vp,
            None => return false,
        };
        let img = match self.kitty_img.id_to_data.get(&image_id) {
            Some(img) => Arc::clone(img),
            None => return false,
        };

        let (row, col) = match (values[0], values[1]) {
            (Some(row), Some(col)) => (row, col),
            (row, col) => {
                // Omitted diacritics continue from the placeholder cell to
                // our left, as in kitty
                match self.kitty_placeholder_to_left(x, y, image_id, vp) {
                    Some((prev_row, prev_col)) => {
                        (row.unwrap_or(prev_row), col.unwrap_or(prev_col + 1))
                    }
                    None => (row.unwrap_or(0), col.unwrap_or(0)),
                }
            }
        };

        // Blank the cell: the placeholder text must never reach the font
        // system, and a reference outside the placement grid displays
        // nothing
        let mut cell = Cell::new(' ', pen.clone());

        if (row as usize) < vp.rows && (col as usize) < vp.cols {
            let cols = vp.cols as f32;
            let rows = vp.rows as f32;
            cell.attrs_mut()
                .attach_image(Box::new(ImageCell::with_z_index(
                    TextureCoordinate::new_f32(col as f32 / cols, row as f32 / rows),
                    TextureCoordinate::new_f32((col + 1) as f32 / cols, (row + 1) as f32 / rows),
                    img,
                    vp.z_index,
                    0,
                    0,
                    0,
                    0,
                    Some(image_id),
                    placement_id,
                )));
        }

        self.screen_mut().set_cell(x, y, &cell, seqno);
        true
    }

    /// If the cell to the left of (x, y) displays a tile of the given
    /// virtual placement, recover its (row, col) from the tile's texture
    /// coordinates, so that placeholders with omitted diacritics can
    /// continue from it.
    fn kitty_placeholder_to_left(
        &mut self,
        x: usize,
        y: VisibleRowIndex,
        image_id: u32,
        vp: KittyVirtualPlacement,
    ) -> Option<(u32, u32)> {
        if x == 0 {
            return None;
        }
        let cell = self.screen_mut().get_cell(x - 1, y)?;
        let images = cell.attrs().images()?;
        let img = images.iter().find(|img| img.image_id() == Some(image_id))?;
        let row = (img.top_left().y.into_inner() * vp.rows as f32).round() as u32;
        let col = (img.top_left().x.into_inner() * vp.cols as f32).round() as u32;
        Some((row, col))
    }

    fn kitty_img_inner(&mut self, img: KittyImage) -> anyhow::Result<()> {
        match self
            .coalesce_kitty_accumulation(img)
            .context("coalesce_kitty_accumulation")?
        {
            KittyImage::TransmitData {
                transmit,
                verbosity,
            } => {
                self.kitty_img_transmit(transmit, verbosity)?;
                Ok(())
            }
            KittyImage::TransmitDataAndDisplay {
                transmit,
                placement,
                verbosity,
            } => {
                log::trace!("TransmitDataAndDisplay {:#?} {:#?}", transmit, placement);
                let image_number = transmit.image_number;
                let image_id = self.kitty_img_transmit(transmit, verbosity)?;
                self.kitty_img_place(Some(image_id), image_number, placement, verbosity)
            }
            _ => anyhow::bail!("impossible KittImage variant"),
        }
    }

    pub(crate) fn kitty_img(&mut self, img: KittyImage) -> anyhow::Result<()> {
        log::trace!("{:?}", img);
        if !self.config.enable_kitty_graphics() {
            return Ok(());
        }
        let verbosity = img.verbosity();
        match img {
            KittyImage::Query { transmit } => match transmit.data.load_data() {
                Ok(_) => {
                    self.kitty_send_response(
                        verbosity,
                        true,
                        transmit.image_id,
                        transmit.image_number,
                        "OK".to_string(),
                    );
                }
                Err(err) => {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        transmit.image_id,
                        transmit.image_number,
                        format!("ERROR:{:#}", err),
                    );
                }
            },
            KittyImage::TransmitData {
                transmit,
                verbosity,
            } => {
                let more_data_follows = transmit.more_data_follows;
                let img = KittyImage::TransmitData {
                    transmit,
                    verbosity,
                };
                if more_data_follows {
                    self.kitty_img.accumulator.push(img);
                } else {
                    self.kitty_img_inner(img)?;
                }
            }
            KittyImage::TransmitDataAndDisplay {
                transmit,
                placement,
                verbosity,
            } => {
                let more_data_follows = transmit.more_data_follows;
                let img = KittyImage::TransmitDataAndDisplay {
                    transmit,
                    placement,
                    verbosity,
                };
                if more_data_follows {
                    self.kitty_img.accumulator.push(img);
                } else {
                    self.kitty_img_inner(img)?;
                }
            }
            KittyImage::Display {
                image_id,
                image_number,
                placement,
                verbosity,
            } => {
                self.kitty_img_place(image_id, image_number, placement, verbosity)?;
            }
            KittyImage::Delete {
                what:
                    KittyImageDelete::ByImageId {
                        image_id,
                        placement_id,
                        delete,
                    },
                verbosity,
            } => {
                log::trace!(
                    "remove a placement: image_id {} placement_id {:?} delete {} verb {:?}",
                    image_id,
                    placement_id,
                    delete,
                    verbosity
                );

                self.kitty_remove_placement(image_id, placement_id);

                if delete {
                    self.kitty_img.remove_data_for_id(image_id);
                }
            }
            KittyImage::Delete {
                what: KittyImageDelete::All { delete },
                verbosity: _,
            } => {
                self.kitty_remove_all_placements(delete);
            }
            KittyImage::Delete { what, verbosity } => {
                log::warn!("unhandled KittyImage::Delete {:?} {:?}", what, verbosity);
            }
            KittyImage::TransmitFrame {
                transmit,
                frame,
                verbosity,
            } => {
                if let Err(err) = self.kitty_frame_transmit(transmit, frame, verbosity) {
                    log::error!("Error {:#} while handling KittyImage::TransmitFrame", err,);
                }
            }
            KittyImage::ComposeFrame { frame, verbosity } => {
                if let Err(err) = self.kitty_frame_compose(frame, verbosity) {
                    log::error!("Error {:#} while handling KittyImage::ComposeFrame", err);
                }
            }
        };

        Ok(())
    }

    fn kitty_remove_placement_from_model(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
        info: PlacementInfo,
    ) {
        let seqno = self.seqno;
        let screen = self.screen_mut();
        let range =
            screen.stable_range(&(info.first_row..info.first_row + info.rows as StableRowIndex));
        for idx in range {
            let line = screen.line_mut(idx);
            for c in line.cells_mut() {
                c.attrs_mut()
                    .detach_image_with_placement(image_id, placement_id);
            }
            line.update_last_change_seqno(seqno);
        }
    }

    /// Placeholder cells referencing a virtual placement can be anywhere
    /// in the visible screen, so detach across all of it rather than a
    /// recorded placement range.
    fn kitty_remove_virtual_placement_from_model(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
    ) {
        let info = PlacementInfo {
            first_row: self.screen().visible_row_to_stable_row(0),
            rows: self.screen().physical_rows,
            cols: 0,
        };
        self.kitty_remove_placement_from_model(image_id, placement_id, info);
    }

    fn kitty_remove_placement(&mut self, image_id: u32, placement_id: Option<u32>) {
        if placement_id.is_some() {
            if let Some(info) = self.kitty_img.placements.remove(&(image_id, placement_id)) {
                log::trace!("removed placement {} {:?}", image_id, placement_id);
                self.kitty_remove_placement_from_model(image_id, placement_id, info);
            }
            if self
                .kitty_img
                .virtual_placements
                .remove(&(image_id, placement_id))
                .is_some()
            {
                self.kitty_remove_virtual_placement_from_model(image_id, placement_id);
            }
        } else {
            let mut to_clear = vec![];
            for (id, p) in self.kitty_img.placements.keys() {
                if *id == image_id {
                    to_clear.push(*p);
                }
            }
            for p in to_clear.into_iter() {
                if let Some(info) = self.kitty_img.placements.remove(&(image_id, p)) {
                    self.kitty_remove_placement_from_model(image_id, p, info);
                }
            }

            let mut to_clear = vec![];
            for (id, p) in self.kitty_img.virtual_placements.keys() {
                if *id == image_id {
                    to_clear.push(*p);
                }
            }
            for p in to_clear.into_iter() {
                self.kitty_img.virtual_placements.remove(&(image_id, p));
                self.kitty_remove_virtual_placement_from_model(image_id, p);
            }
        }

        log::trace!(
            "after remove: there are {} placements, {} images, {} memory",
            self.kitty_img.placements.len(),
            self.kitty_img.id_to_data.len(),
            self.kitty_img.used_memory,
        );
    }

    pub(crate) fn kitty_remove_all_placements(&mut self, delete: bool) {
        for ((image_id, p), info) in std::mem::take(&mut self.kitty_img.placements).into_iter() {
            self.kitty_remove_placement_from_model(image_id, p, info);
        }
        for ((image_id, p), _) in std::mem::take(&mut self.kitty_img.virtual_placements).into_iter()
        {
            self.kitty_remove_virtual_placement_from_model(image_id, p);
        }
        if delete {
            self.kitty_img.id_to_data.clear();
            self.kitty_img.used_memory = 0;
            self.kitty_img.number_to_id.clear();
        }
    }

    fn kitty_send_response(
        &mut self,
        verbosity: KittyImageVerbosity,
        success: bool,
        image_id: Option<u32>,
        image_no: Option<u32>,
        message: String,
    ) {
        match verbosity {
            KittyImageVerbosity::Verbose => {}
            KittyImageVerbosity::OnlyErrors => {
                if success {
                    return;
                }
            }
            KittyImageVerbosity::Quiet => {
                return;
            }
        }

        log::trace!("Query Response: {}", message);

        match (image_id, image_no) {
            (Some(id), Some(no)) => {
                write!(self.writer, "\x1b_GI={},i={};{}\x1b\\", no, id, message).ok();
            }
            (Some(id), None) => {
                write!(self.writer, "\x1b_Gi={};{}\x1b\\", id, message).ok();
            }
            (None, Some(no)) => {
                write!(self.writer, "\x1b_GI={};{}\x1b\\", no, message).ok();
            }
            (None, None) => {
                write!(self.writer, "\x1b_G{}\x1b\\", message).ok();
            }
        }
        self.writer.flush().ok();
    }

    fn kitty_frame_compose(
        &mut self,
        frame: KittyImageFrameCompose,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let image_id = match frame.image_number {
            Some(no) => match self.kitty_img.number_to_id.get(&no) {
                Some(id) => *id,
                None => {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        frame.image_id,
                        frame.image_number,
                        "ENOENT".to_string(),
                    );
                    anyhow::bail!("no such image_number {}", no);
                }
            },
            None => frame.image_id.ok_or_else(|| {
                self.kitty_send_response(
                    verbosity,
                    false,
                    frame.image_id,
                    frame.image_number,
                    "ENOENT".to_string(),
                );
                anyhow::anyhow!("no image_id")
            })?,
        };

        let src_frame = frame.source_frame.ok_or_else(|| {
            self.kitty_send_response(
                verbosity,
                false,
                frame.image_id,
                frame.image_number,
                "ENOENT".to_string(),
            );
            anyhow::anyhow!("missing source frame")
        })? as usize;
        let target_frame = frame.target_frame.ok_or_else(|| {
            self.kitty_send_response(
                verbosity,
                false,
                frame.image_id,
                frame.image_number,
                "ENOENT".to_string(),
            );
            anyhow::anyhow!("missing target frame")
        })? as usize;

        let img = self
            .kitty_img
            .id_to_data
            .get(&image_id)
            .ok_or_else(|| anyhow::anyhow!("invalid image id {}", image_id))?;

        let mut img = img.data();
        match &mut *img {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("invalid image type")
            }
            ImageDataType::Rgba8 {
                width,
                height,
                data,
                hash,
            } => {
                anyhow::ensure!(
                    src_frame == target_frame && src_frame == 1,
                    "src_frame={} target_frame={} but there is only a single frame",
                    src_frame,
                    target_frame
                );

                let src = clip_view(
                    *width,
                    *height,
                    data.as_mut_slice(),
                    frame.src_x,
                    frame.src_y,
                    frame.w,
                    frame.h,
                )?;

                let mut dest: ImageBuffer<Rgba<u8>, &mut [u8]> =
                    ImageBuffer::from_raw(*width, *height, data.as_mut_slice())
                        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

                blit(
                    &mut dest,
                    &src,
                    frame.x.unwrap_or(0),
                    frame.y.unwrap_or(0),
                    frame.composition_mode,
                )?;

                drop(dest);

                *hash = ImageDataType::hash_bytes(data);
            }
            ImageDataType::AnimRgba8 {
                width,
                height,
                frames,
                hashes,
                ..
            } => {
                anyhow::ensure!(
                    src_frame > 0 && src_frame <= frames.len(),
                    "src_frame {} is out of range",
                    src_frame
                );
                anyhow::ensure!(
                    target_frame > 0 && target_frame <= frames.len(),
                    "target_frame {} is out of range",
                    target_frame
                );

                let src = clip_view(
                    *width,
                    *height,
                    frames[src_frame - 1].as_mut_slice(),
                    frame.src_x,
                    frame.src_y,
                    frame.w,
                    frame.h,
                )?;

                let mut dest: ImageBuffer<Rgba<u8>, &mut [u8]> =
                    ImageBuffer::from_raw(*width, *height, frames[target_frame - 1].as_mut_slice())
                        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

                blit(
                    &mut dest,
                    &src,
                    frame.x.unwrap_or(0),
                    frame.y.unwrap_or(0),
                    frame.composition_mode,
                )?;

                drop(dest);
                hashes[target_frame - 1] = ImageDataType::hash_bytes(&frames[target_frame - 1]);
            }
        }

        Ok(())
    }

    fn kitty_frame_transmit(
        &mut self,
        mut transmit: KittyImageTransmit,
        frame: KittyImageFrame,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        if let Some(no) = transmit.image_number.take() {
            match self.kitty_img.number_to_id.get(&no) {
                Some(id) => {
                    transmit.image_id.replace(*id);
                }
                None => {
                    transmit.image_number.replace(no);
                }
            }
        }

        let (image_id, image_number, img) = self.kitty_img_transmit_inner(transmit)?;

        let img = match img.decode() {
            ImageDataType::Rgba8 {
                data,
                width,
                height,
                ..
            } => RgbaImage::from_vec(width, height, data)
                .ok_or_else(|| anyhow::anyhow!("data isn't rgba8"))?,
            wat => anyhow::bail!("data isn't rgba8 {:?}", wat),
        };

        let background_pixel = frame.background_pixel.unwrap_or(0);
        let background_pixel = Rgba([
            ((background_pixel >> 24) & 0xff) as u8,
            ((background_pixel >> 16) & 0xff) as u8,
            ((background_pixel >> 8) & 0xff) as u8,
            (background_pixel & 0xff) as u8,
        ]);

        let anim = match self.kitty_img.id_to_data.get(&image_id) {
            Some(anim) => anim,
            None => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    "ENOENT".to_string(),
                );
                anyhow::bail!(
                    "no matching image id {} in id_to_data for image_number {:?}",
                    image_id,
                    image_number
                )
            }
        };

        let mut anim = anim.data();
        let x = frame.x.unwrap_or(0);
        let y = frame.y.unwrap_or(0);
        let frame_gap = Duration::from_millis(match frame.duration_ms {
            None | Some(0) => 40,
            Some(n) => n.into(),
        });

        match &mut *anim {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("Expected decoded image for image id {}", image_id)
            }
            ImageDataType::Rgba8 {
                data,
                width,
                height,
                hash,
            } => {
                let base_frame = match frame.base_frame {
                    Some(1) => Some(1),
                    None => None,
                    Some(n) => anyhow::bail!(
                        "attempted to copy frame {} but there is only a single frame",
                        n
                    ),
                };

                match frame.frame_number {
                    Some(1) => {
                        // Edit in place
                        let len = data.len();
                        let mut anim_img: ImageBuffer<Rgba<u8>, &mut [u8]> =
                            ImageBuffer::from_raw(*width, *height, data.as_mut_slice())
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "ImageBuffer::from_raw failed for single \
                                         frame of {}x{} ({} bytes)",
                                        width,
                                        height,
                                        len
                                    )
                                })?;

                        blit(&mut anim_img, &img, x, y, frame.composition_mode)?;

                        drop(anim_img);
                        *hash = ImageDataType::hash_bytes(data);
                    }
                    Some(2) | None => {
                        // Create a second frame

                        let mut new_frame = if base_frame.is_some() {
                            RgbaImage::from_vec(*width, *height, data.clone()).unwrap()
                        } else {
                            RgbaImage::from_pixel(*width, *height, background_pixel)
                        };

                        blit(&mut new_frame, &img, x, y, frame.composition_mode)?;

                        let new_frame_data = new_frame.into_vec();
                        let new_frame_hash = ImageDataType::hash_bytes(&new_frame_data);

                        let frames = vec![std::mem::take(data), new_frame_data];
                        let durations = vec![Duration::from_millis(0), frame_gap];
                        let hashes = vec![*hash, new_frame_hash];

                        *anim = ImageDataType::AnimRgba8 {
                            width: *width,
                            height: *height,
                            frames,
                            durations,
                            hashes,
                        };
                    }
                    Some(n) => anyhow::bail!(
                        "attempted to edit frame {} but there is only a single frame",
                        n
                    ),
                }
            }
            ImageDataType::AnimRgba8 {
                width,
                height,
                frames,
                durations,
                hashes,
            } => {
                let frame_no = frame.frame_number.unwrap_or(frames.len() as u32 + 1);
                if frame_no == frames.len() as u32 + 1 {
                    // Append a new frame

                    let mut new_frame = match frame.base_frame {
                        None => RgbaImage::from_pixel(*width, *height, background_pixel),
                        Some(n) => {
                            let n = n as usize;
                            anyhow::ensure!(
                                n > 0 && n <= frames.len(),
                                "attempted to copy frame {} which is outside range 1-{}",
                                n,
                                frames.len()
                            );
                            RgbaImage::from_vec(*width, *height, frames[n - 1].clone()).unwrap()
                        }
                    };

                    blit(&mut new_frame, &img, x, y, frame.composition_mode)?;

                    let new_frame_data = new_frame.into_vec();
                    let new_frame_hash = ImageDataType::hash_bytes(&new_frame_data);

                    frames.push(new_frame_data);
                    hashes.push(new_frame_hash);
                    durations.push(frame_gap);
                } else {
                    anyhow::ensure!(
                        frame_no > 0 && frame_no <= frames.len() as u32,
                        "attempted to edit frame {} which is outside range 1-{}",
                        frame_no,
                        frames.len()
                    );

                    let frame_no = frame_no as usize;

                    let len = frames[frame_no - 1].len();
                    let mut anim_img: ImageBuffer<Rgba<u8>, &mut [u8]> =
                        ImageBuffer::from_raw(*width, *height, frames[frame_no - 1].as_mut_slice())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "ImageBuffer::from_raw failed for single \
                                         frame of {}x{} ({} bytes)",
                                    width,
                                    height,
                                    len
                                )
                            })?;

                    blit(&mut anim_img, &img, x, y, frame.composition_mode)?;

                    drop(anim_img);
                    hashes[frame_no - 1] = ImageDataType::hash_bytes(&frames[frame_no - 1]);
                }
            }
        }

        Ok(())
    }

    fn kitty_img_transmit_inner(
        &mut self,
        transmit: KittyImageTransmit,
    ) -> anyhow::Result<(u32, Option<u32>, ImageDataType)> {
        log::trace!("transmit {:?}", transmit);
        let (id, no) = match (transmit.image_id, transmit.image_number) {
            (Some(_), Some(_)) => {
                // TODO: send an EINVAL error back here
                anyhow::bail!("cannot use both i= and I= in the same request");
            }
            (None, None) => {
                // Assume image id 0
                (0, None)
            }
            (Some(id), None) => (id, None),
            (None, Some(no)) => {
                let id = self.kitty_img.max_image_id + 1;
                self.kitty_img.number_to_id.insert(no, id);
                (id, Some(no))
            }
        };

        let data = transmit
            .data
            .load_data()
            .context("data should have been materialized in coalesce_kitty_accumulation")?;

        let data = match transmit.compression {
            KittyImageCompression::None => data,
            KittyImageCompression::Deflate => {
                miniz_oxide::inflate::decompress_to_vec_zlib(&data)
                    .map_err(|e| anyhow::anyhow!("decompressing data: {:?}", e))?
            }
        };

        let img = match transmit.format {
            None | Some(KittyImageFormat::Rgba) | Some(KittyImageFormat::Rgb) => {
                let (width, height) = match (transmit.width, transmit.height) {
                    (Some(w), Some(h)) => (w, h),
                    _ => {
                        anyhow::bail!("missing width/height info for kitty img");
                    }
                };

                check_image_dimensions(width, height)?;

                let data = match transmit.format {
                    Some(KittyImageFormat::Rgb) => {
                        let img = DynamicImage::ImageRgb8(
                            RgbImage::from_vec(width, height, data)
                                .ok_or_else(|| anyhow::anyhow!("failed to decode image"))?,
                        );
                        let img = img.into_rgba8();
                        img.into_vec()
                    }
                    _ => data,
                };

                anyhow::ensure!(
                    width * height * 4 == data.len() as u32,
                    "transmit data len is {} but it doesn't match width*height*4 {}x{}x4 = {}",
                    data.len(),
                    width,
                    height,
                    width * height * 4
                );

                ImageDataType::new_single_frame(width, height, data)
            }
            Some(KittyImageFormat::Png) => {
                let info = dimensions(&data)?;
                check_image_dimensions(info.width, info.height)?;
                let decoded = image::load_from_memory(&data).context("decode png")?;
                let (width, height) = decoded.dimensions();
                let data = decoded.into_rgba8().into_vec();
                ImageDataType::new_single_frame(width, height, data)
            }
        };

        Ok((id, no, img))
    }

    fn kitty_img_transmit(
        &mut self,
        transmit: KittyImageTransmit,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<u32> {
        let (image_id, image_number, img) = self.kitty_img_transmit_inner(transmit)?;
        self.kitty_img.max_image_id = self.kitty_img.max_image_id.max(image_id);

        let img = self
            .raw_image_to_image_data(img)
            .context("storing image data")?;
        self.kitty_img.record_id_to_data(image_id, img);

        if image_number.is_some() {
            self.kitty_send_response(
                verbosity,
                true,
                Some(image_id),
                image_number,
                "OK".to_string(),
            );
        }

        Ok(image_id)
    }

    fn coalesce_kitty_accumulation(&mut self, img: KittyImage) -> anyhow::Result<KittyImage> {
        if self.kitty_img.accumulator.is_empty() {
            Ok(img)
        } else {
            let mut data = vec![];
            let mut trans;
            let place;
            let final_verbosity = img.verbosity();

            self.kitty_img.accumulator.push(img);

            let mut empty_data = KittyImageData::Direct(String::new());
            match self.kitty_img.accumulator.remove(0) {
                KittyImage::TransmitData { transmit, .. } => {
                    trans = transmit;
                    place = None;
                    std::mem::swap(&mut empty_data, &mut trans.data);
                }
                KittyImage::TransmitDataAndDisplay {
                    transmit,
                    placement,
                    ..
                } => {
                    place = Some(placement);
                    trans = transmit;
                    std::mem::swap(&mut empty_data, &mut trans.data);
                }
                _ => unreachable!(),
            }
            data.push(empty_data);

            for item in self.kitty_img.accumulator.drain(..) {
                match item {
                    KittyImage::TransmitData { transmit, .. }
                    | KittyImage::TransmitDataAndDisplay { transmit, .. } => {
                        data.push(transmit.data);
                    }
                    _ => unreachable!(),
                }
            }

            let mut b64_decoded = vec![];
            for mut data in data.into_iter() {
                match &mut data {
                    KittyImageData::DirectBin(b) => {
                        b64_decoded.append(b);
                    }
                    KittyImageData::Direct(b) => {
                        if !b.is_empty() {
                            b64_decoded.append(&mut data.load_data()?);
                        }
                    }
                    data => {
                        anyhow::bail!("expected data chunks to be Direct data, found {:#?}", data)
                    }
                }
            }

            trans.data = KittyImageData::DirectBin(b64_decoded);

            if let Some(placement) = place {
                Ok(KittyImage::TransmitDataAndDisplay {
                    transmit: trans,
                    placement,
                    verbosity: final_verbosity,
                })
            } else {
                Ok(KittyImage::TransmitData {
                    transmit: trans,
                    verbosity: final_verbosity,
                })
            }
        }
    }
}

/// Make a copy of the source region.
/// Ideally we wouldn't need this, but Rust's mutability rules
/// make it very awkward to mutably reference a frame while
/// an immutable reference exists to a separate frame.
fn clip_view(
    width: u32,
    height: u32,
    data: &mut [u8],
    src_x: Option<u32>,
    src_y: Option<u32>,
    view_width: Option<u32>,
    view_height: Option<u32>,
) -> anyhow::Result<RgbaImage> {
    let src = ImageBuffer::from_raw(width, height, data)
        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

    let src_x = src_x.unwrap_or(0);
    let src_y = src_y.unwrap_or(0);

    let view_width = view_width.unwrap_or(width);
    let view_height = view_height.unwrap_or(height);

    let (view_width, view_height) =
        image::imageops::overlay_bounds((width, height), (view_width, view_height), src_x, src_y);

    let view = src.view(src_x, src_y, view_width, view_height);

    let mut tmp = RgbaImage::new(view_width, view_height);
    tmp.copy_from(&*view, 0, 0).context("copy source image")?;
    Ok(tmp)
}

fn blit<D, S, P>(
    dest: &mut D,
    src: &S,
    x: u32,
    y: u32,
    mode: KittyFrameCompositionMode,
) -> anyhow::Result<()>
where
    D: GenericImage<Pixel = P>,
    S: GenericImageView<Pixel = P>,
{
    match mode {
        KittyFrameCompositionMode::Overwrite => {
            ::image::imageops::replace(dest, src, x.into(), y.into());
        }
        KittyFrameCompositionMode::AlphaBlending => {
            ::image::imageops::overlay(dest, src, x.into(), y.into());
        }
    }
    Ok(())
}
