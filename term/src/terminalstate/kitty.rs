use crate::terminalstate::image::*;
use crate::terminalstate::kitty_placeholder::{
    diacritic_to_num, KittyPlaceholderRun, IMAGE_PLACEHOLDER_CHAR,
};
use crate::terminalstate::{ImageAttachParams, PlacementInfo};
use crate::{StableRowIndex, TerminalState};
use ::image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, RgbImage, Rgba, RgbaImage,
};
use anyhow::Context;
use ordered_float::NotNan;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use wezterm_cell::image::{ImageCell, ImageDataType};
use wezterm_cell::{Cell, CellAttributes};
use wezterm_escape_parser::apc::{
    KittyAnimationState, KittyFrameCompositionMode, KittyImage, KittyImageAnimate,
    KittyImageCompression, KittyImageData, KittyImageDelete, KittyImageFormat, KittyImageFrame,
    KittyImageFrameCompose, KittyImagePlacement, KittyImageTransmit, KittyImageVerbosity,
};
use wezterm_surface::change::ImageData;
use wezterm_surface::TextureCoordinate;

#[derive(Debug, Default)]
pub struct KittyImageState {
    accumulator: Vec<KittyImage>,
    max_image_id: u32,
    number_to_id: HashMap<u32, u32>,
    id_to_data: HashMap<u32, Arc<ImageData>>,
    /// Generation at which each image id was last (re)transmitted, so
    /// that over-budget pruning can release the oldest images first.
    id_generation: HashMap<u32, u64>,
    next_generation: u64,
    placements: HashMap<(u32, Option<u32>), PlacementInfo>,
    used_memory: usize,
    /// Image ids whose payload was released by the storage budget
    /// prune and written to the on-disk image cache, mapped to the
    /// content hash under which the payload was spilled (and the
    /// generation at release time, used to bound the map).  A later
    /// placement of one of these ids restores the payload from disk
    /// instead of observing ENOENT.
    spilled: HashMap<u32, ([u8; 32], u64)>,
}

/// Ceiling on the number of remembered spilled ids.  The map itself is
/// tiny (a hash and a generation per entry); the cap keeps it bounded
/// for adversarial clients that transmit huge numbers of images.
const MAX_SPILLED_IDS: usize = 4096;

impl KittyImageState {
    fn remove_data_for_id(&mut self, image_id: u32) {
        self.id_generation.remove(&image_id);
        if let Some(data) = self.id_to_data.remove(&image_id) {
            self.used_memory = self.used_memory.saturating_sub(data.len());
        }
    }

    fn record_id_to_data(&mut self, image_id: u32, data: Arc<ImageData>) {
        // Replacing the payload for an id must first drop the accounting
        // for the previous payload.  This also covers image id 0, which
        // clients re-transmit for every frame, and which previously
        // double-counted its payloads in used_memory forever.
        self.remove_data_for_id(image_id);
        // Fresh data for the id invalidates any spilled record: the
        // payload in memory is authoritative from here on.
        self.spilled.remove(&image_id);
        self.used_memory += data.len();
        self.id_to_data.insert(image_id, data);
        self.id_generation.insert(image_id, self.next_generation);
        self.next_generation += 1;
    }

    /// Remember that image_id was released to the disk cache under
    /// `hash`, so that a later placement can restore it.
    fn record_spilled(&mut self, image_id: u32, hash: [u8; 32], generation: u64) {
        if self.spilled.len() >= MAX_SPILLED_IDS && !self.spilled.contains_key(&image_id) {
            // Forget the oldest record.  The disk file is content
            // addressed and may be shared; it is left to the cache's
            // own budget.
            let victim = self
                .spilled
                .iter()
                .min_by_key(|(_, (_, gen))| *gen)
                .map(|(id, _)| *id);
            if let Some(victim) = victim {
                self.spilled.remove(&victim);
            }
        }
        self.spilled.insert(image_id, (hash, generation));
    }

    /// Test-only views of the storage accounting
    #[cfg(test)]
    pub(crate) fn stored_image_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.id_to_data.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Image ids whose payloads live only in the disk cache; for tests.
    #[cfg(test)]
    pub(crate) fn spilled_image_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.spilled.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Image ids whose stored payloads are currently swapped out to
    /// the image disk cache; for tests.
    #[cfg(test)]
    pub(crate) fn disk_backed_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .id_to_data
            .iter()
            .filter(|(_, data)| data.is_disk_backed())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The stored image data for an id; for tests.
    #[cfg(test)]
    pub(crate) fn image_data(&self, image_id: u32) -> Option<Arc<ImageData>> {
        self.id_to_data.get(&image_id).map(Arc::clone)
    }

    #[cfg(test)]
    pub(crate) fn used_memory(&self) -> usize {
        self.used_memory
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
            None => {
                let no = image_number.ok_or_else(|| {
                    anyhow::Error::new(KittyProtocolError {
                        code: "EINVAL",
                        detail: "no image_id or image_number specified".to_string(),
                    })
                })?;
                *self.kitty_img.number_to_id.get(&no).ok_or_else(|| {
                    anyhow::Error::new(KittyProtocolError {
                        code: "ENOENT",
                        detail: format!("image_number {:?} has no matching image id", no),
                    })
                })?
            }
        };

        log::trace!(
            "kitty_img_place image_id {:?} image_no {:?} placement {:?} verb {:?}",
            image_id,
            image_number,
            placement,
            verbosity
        );

        if placement.unicode_placeholder {
            // A virtual placement reserves a rectangle for unicode
            // placeholder text but does not itself touch any cells.
            // It is rendered when U+10EEEE characters referencing it
            // are printed.
            if placement.parent_image_id.is_some() {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "EINVAL",
                    detail: "a virtual placement cannot also be a relative placement".to_string(),
                }));
            }
            let (cols, rows) = match (placement.columns, placement.rows) {
                (Some(cols), Some(rows)) if cols > 0 && rows > 0 => (cols as usize, rows as usize),
                _ => {
                    return Err(anyhow::Error::new(KittyProtocolError {
                        code: "EINVAL",
                        detail: "a virtual placement needs both c= and r=".to_string(),
                    }))
                }
            };
            let first_row = self.screen().visible_row_to_stable_row(self.cursor.y);
            self.kitty_img.placements.insert(
                (image_id, placement.placement_id),
                PlacementInfo {
                    first_row,
                    first_col: 0,
                    rows: 0,
                    cols: 0,
                    z_index: placement.z_index.unwrap_or(0),
                    parent: None,
                    parent_offsets: (0, 0),
                    virtual_rect: Some((cols, rows)),
                },
            );
            return Ok(());
        }

        if let Some(parent_image_id) = placement.parent_image_id {
            return self.kitty_img_place_relative(
                image_id,
                image_number,
                placement,
                parent_image_id,
            );
        }

        if image_id != 0 {
            // Re-placing detaches only this placement; relative
            // children are moved along below rather than deleted
            let keys: Vec<Option<u32>> = if placement.placement_id.is_some() {
                vec![placement.placement_id]
            } else {
                self.kitty_img
                    .placements
                    .keys()
                    .filter(|(id, _)| *id == image_id)
                    .map(|(_, p)| *p)
                    .collect()
            };
            for pid in keys {
                if let Some(info) = self.kitty_img.placements.remove(&(image_id, pid)) {
                    self.kitty_remove_placement_from_model(image_id, pid, info);
                }
            }
        }
        let img = match self.kitty_img_lookup_or_restore(image_id) {
            Some(img) => img,
            None => {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "ENOENT",
                    detail: format!(
                        "no matching image id {} in id_to_data for image_number {:?}",
                        image_id, image_number
                    ),
                }))
            }
        };

        let (image_width, image_height) = img.data().dimensions()?;

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
            origin: None,
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

        // Relative placements follow their parent when it moves
        self.kitty_relattach_children((image_id, placement.placement_id));

        Ok(())
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
            KittyImage::Query { transmit } => {
                // A failure to read from a file or shared memory is
                // reported as EBADF, per the spec; anything else is a
                // problem with the payload itself (EINVAL).
                let from_storage = matches!(
                    transmit.data,
                    KittyImageData::File { .. }
                        | KittyImageData::TemporaryFile { .. }
                        | KittyImageData::SharedMem { .. }
                );
                let (image_id, image_number) = (transmit.image_id, transmit.image_number);
                match transmit.data.load_data() {
                    Ok(_) => {
                        self.kitty_send_response(
                            verbosity,
                            true,
                            image_id,
                            image_number,
                            None,
                            "OK".to_string(),
                        );
                    }
                    Err(err) => {
                        let message = if from_storage {
                            format!("EBADF:{:#}", err)
                        } else {
                            format!("EINVAL:{:#}", err)
                        };
                        self.kitty_send_response(
                            verbosity,
                            false,
                            image_id,
                            image_number,
                            None,
                            message,
                        );
                    }
                }
            }
            KittyImage::TransmitData {
                transmit,
                verbosity,
            } => {
                let more_data_follows = transmit.more_data_follows;
                let image_id = transmit.image_id;
                let image_number = transmit.image_number;
                let img = KittyImage::TransmitData {
                    transmit,
                    verbosity,
                };
                if more_data_follows {
                    self.kitty_img.accumulator.push(img);
                } else if let Err(err) = self.kitty_img_inner(img) {
                    log::error!("Error {:#} while handling KittyImage::TransmitData", err);
                    self.kitty_reply_error(verbosity, image_id, image_number, None, &err);
                }
            }
            KittyImage::TransmitDataAndDisplay {
                transmit,
                placement,
                verbosity,
            } => {
                let more_data_follows = transmit.more_data_follows;
                let image_id = transmit.image_id;
                let image_number = transmit.image_number;
                let placement_id = placement.placement_id;
                let img = KittyImage::TransmitDataAndDisplay {
                    transmit,
                    placement,
                    verbosity,
                };
                if more_data_follows {
                    self.kitty_img.accumulator.push(img);
                } else if let Err(err) = self.kitty_img_inner(img) {
                    log::error!(
                        "Error {:#} while handling KittyImage::TransmitDataAndDisplay",
                        err
                    );
                    self.kitty_reply_error(verbosity, image_id, image_number, placement_id, &err);
                }
            }
            KittyImage::Display {
                image_id,
                image_number,
                placement,
                verbosity,
            } => {
                let placement_id = placement.placement_id;
                match self.kitty_img_place(image_id, image_number, placement, verbosity) {
                    Ok(()) => {
                        // Placement requests are acknowledged when they
                        // identify the image they placed
                        if image_id.is_some() || image_number.is_some() {
                            self.kitty_send_response(
                                verbosity,
                                true,
                                image_id,
                                image_number,
                                placement_id,
                                "OK".to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        log::error!("Error {:#} while handling KittyImage::Display", err);
                        self.kitty_reply_error(
                            verbosity,
                            image_id,
                            image_number,
                            placement_id,
                            &err,
                        );
                    }
                }
            }
            KittyImage::Delete { what, verbosity } => {
                self.kitty_img_delete(what, verbosity);
            }
            KittyImage::TransmitFrame {
                transmit,
                frame,
                verbosity,
            } => {
                let image_number = transmit.image_number;
                match self.kitty_frame_transmit(transmit, frame, verbosity) {
                    Ok(image_id) => {
                        if let Some(no) = image_number {
                            self.kitty_send_response(
                                verbosity,
                                true,
                                Some(image_id),
                                Some(no),
                                None,
                                "OK".to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        log::error!("Error {:#} while handling KittyImage::TransmitFrame", err);
                        self.kitty_reply_error(verbosity, None, image_number, None, &err);
                    }
                }
            }
            KittyImage::ComposeFrame { frame, verbosity } => {
                let image_id = frame.image_id;
                let image_number = frame.image_number;
                if let Err(err) = self.kitty_frame_compose(frame, verbosity) {
                    log::error!("Error {:#} while handling KittyImage::ComposeFrame", err);
                    self.kitty_reply_error(verbosity, image_id, image_number, None, &err);
                }
            }
            KittyImage::Animate { control, verbosity } => {
                let image_id = control.image_id;
                let image_number = control.image_number;
                match self.kitty_animate(control) {
                    Ok(()) => {
                        if image_id.is_some() || image_number.is_some() {
                            self.kitty_send_response(
                                verbosity,
                                true,
                                image_id,
                                image_number,
                                None,
                                "OK".to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        log::error!("Error {:#} while handling KittyImage::Animate", err);
                        self.kitty_reply_error(verbosity, image_id, image_number, None, &err);
                    }
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

    /// Place an image relative to another placement (P=/Q=/H=/V=).
    /// The child follows its parent when the parent is re-placed and
    /// is deleted together with it.
    fn kitty_img_place_relative(
        &mut self,
        image_id: u32,
        image_number: Option<u32>,
        placement: KittyImagePlacement,
        parent_image_id: u32,
    ) -> anyhow::Result<()> {
        let key = (image_id, placement.placement_id);
        let parent_key = (parent_image_id, placement.parent_placement_id);

        if key == parent_key {
            return Err(anyhow::Error::new(KittyProtocolError {
                code: "EINVAL",
                detail: "a placement cannot be its own parent".to_string(),
            }));
        }

        let parent_info = match self.kitty_img.placements.get(&parent_key) {
            Some(info) => *info,
            None => {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "ENOPARENT",
                    detail: format!(
                        "no parent placement {}:{:?}",
                        parent_image_id, placement.parent_placement_id
                    ),
                }))
            }
        };

        // Walk the ancestry: reject cycles and chains that are too deep.
        // kitty guarantees at least a depth of 8.
        const PARENT_DEPTH_LIMIT: usize = 8;
        let mut ancestor = parent_key;
        for depth in 0..PARENT_DEPTH_LIMIT {
            if ancestor == key {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "ECYCLE",
                    detail: "this parent reference creates a cycle".to_string(),
                }));
            }
            ancestor = match self
                .kitty_img
                .placements
                .get(&ancestor)
                .and_then(|i| i.parent)
            {
                Some(next) => next,
                None => break,
            };
            if depth + 1 == PARENT_DEPTH_LIMIT {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "ETOODEEP",
                    detail: "too many levels of parent references".to_string(),
                }));
            }
        }

        // Where the parent currently sits
        let (parent_x, parent_y) = if parent_info.virtual_rect.is_some() {
            // A virtual parent sits at the top left of the placeholder
            // cells that currently reference it
            match self.kitty_find_virtual_anchor(parent_key) {
                Some(anchor) => anchor,
                None => {
                    return Err(anyhow::Error::new(KittyProtocolError {
                        code: "ENOPARENT",
                        detail: "the parent virtual placement is not on screen".to_string(),
                    }))
                }
            }
        } else {
            let viewport_top = self.screen().visible_row_to_stable_row(0);
            (
                parent_info.first_col as i64,
                (parent_info.first_row - viewport_top) as i64,
            )
        };

        let h = placement.h_offset.unwrap_or(0);
        let v = placement.v_offset.unwrap_or(0);
        let origin = (parent_x + h, parent_y + v);

        let img = match self.kitty_img_lookup_or_restore(image_id) {
            Some(img) => img,
            None => {
                return Err(anyhow::Error::new(KittyProtocolError {
                    code: "ENOENT",
                    detail: format!(
                        "no matching image id {} in id_to_data for image_number {:?}",
                        image_id, image_number
                    ),
                }))
            }
        };

        let (image_width, image_height) = img.data().dimensions()?;

        // Remove the previous incarnation of this placement, children
        // included; they are re-attached below
        self.kitty_remove_placement_tree(key);

        let mut info = self.assign_image_to_cells(ImageAttachParams {
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
            do_not_move_cursor: true,
            origin: Some(origin),
        })?;
        info.parent = Some(parent_key);
        info.parent_offsets = (h, v);
        self.kitty_img.placements.insert(key, info);

        Ok(())
    }

    /// The top left cell of the placeholder cells currently rendering
    /// a virtual placement, in (column, viewport row) form.
    fn kitty_find_virtual_anchor(&mut self, key: (u32, Option<u32>)) -> Option<(i64, i64)> {
        let mut best: Option<(i64, i64)> = None;
        let rows = self.screen().physical_rows as i64;
        for y in 0..rows {
            let stable = self.screen().visible_row_to_stable_row(y);
            let line = self.screen_mut().line_mut(stable as usize);
            for cell in line.visible_cells() {
                if let Some(images) = cell.attrs().images() {
                    for img in images {
                        if img.image_id() == Some(key.0)
                            && (key.1.is_none() || img.placement_id() == key.1)
                        {
                            let cx = cell.cell_index() as i64;
                            best = Some(match best {
                                None => (cx, y),
                                Some((mx, my)) => (mx.min(cx), my.min(y)),
                            });
                        }
                    }
                }
            }
        }
        best
    }

    /// Remove a placement and, recursively, the placements that hang
    /// off of it; their lifetime is tied to their parent.
    fn kitty_remove_placement_tree(&mut self, key: (u32, Option<u32>)) {
        let children: Vec<(u32, Option<u32>)> = self
            .kitty_img
            .placements
            .iter()
            .filter(|(_, info)| info.parent == Some(key))
            .map(|(k, _)| *k)
            .collect();
        if let Some(info) = self.kitty_img.placements.remove(&key) {
            self.kitty_remove_placement_from_model(key.0, key.1, info);
        }
        for child in children {
            self.kitty_remove_placement_tree(child);
        }
    }

    /// Re-attach the placements that hang off of `key` so that they
    /// follow a parent that was just moved.
    fn kitty_relattach_children(&mut self, key: (u32, Option<u32>)) {
        let children: Vec<((u32, Option<u32>), PlacementInfo)> = self
            .kitty_img
            .placements
            .iter()
            .filter(|(_, info)| info.parent == Some(key))
            .map(|(k, info)| (*k, *info))
            .collect();
        for (child_key, child_info) in children {
            let parent = match self.kitty_img.placements.get(&key) {
                Some(info) => *info,
                None => return,
            };
            let (parent_x, parent_y) = if parent.virtual_rect.is_some() {
                match self.kitty_find_virtual_anchor(key) {
                    Some(anchor) => anchor,
                    None => continue,
                }
            } else {
                let viewport_top = self.screen().visible_row_to_stable_row(0);
                (
                    parent.first_col as i64,
                    (parent.first_row - viewport_top) as i64,
                )
            };
            let (h, v) = child_info.parent_offsets;
            let origin = (parent_x + h, parent_y + v);

            let data = match self.kitty_img_lookup_or_restore(child_key.0) {
                Some(data) => data,
                None => continue,
            };
            let (image_width, image_height) = match data.data().dimensions() {
                Ok(d) => d,
                Err(_) => continue,
            };

            self.kitty_remove_placement_from_model(child_key.0, child_key.1, child_info);

            let info = match self.assign_image_to_cells(ImageAttachParams {
                image_width,
                image_height,
                source_width: None,
                source_height: None,
                source_origin_x: 0,
                source_origin_y: 0,
                cell_padding_left: 0,
                cell_padding_top: 0,
                data,
                style: ImageAttachStyle::Kitty,
                z_index: child_info.z_index,
                columns: None,
                rows: None,
                image_id: Some(child_key.0),
                placement_id: child_key.1,
                do_not_move_cursor: true,
                origin: Some(origin),
            }) {
                Ok(info) => info,
                Err(err) => {
                    log::error!("re-attaching relative placement: {:#}", err);
                    continue;
                }
            };
            // Preserve the shape the child was created with
            let mut info = info;
            info.parent = Some(key);
            info.parent_offsets = (h, v);
            self.kitty_img.placements.insert(child_key, info);

            // And its own children follow in turn
            self.kitty_relattach_children(child_key);
        }
    }

    fn kitty_remove_placement(&mut self, image_id: u32, placement_id: Option<u32>) {
        if placement_id.is_some() {
            log::trace!("removed placement {} {:?}", image_id, placement_id);
            self.kitty_remove_placement_tree((image_id, placement_id));
        } else {
            let to_clear: Vec<Option<u32>> = self
                .kitty_img
                .placements
                .keys()
                .filter(|(id, _)| *id == image_id)
                .map(|(_, p)| *p)
                .collect();
            for p in to_clear.into_iter() {
                self.kitty_remove_placement_tree((image_id, p));
            }
        }

        log::trace!(
            "after remove: there are {} placements, {} images, {} memory",
            self.kitty_img.placements.len(),
            self.kitty_img.id_to_data.len(),
            self.kitty_img.used_memory,
        );
    }

    /// d=a/A: virtual placements are only ever deleted by
    /// d=i/I/r/R/n/N, so they (and the data of images that only they
    /// reference) survive a delete-all.
    pub(crate) fn kitty_remove_all_placements(&mut self, delete: bool) {
        let taken = std::mem::take(&mut self.kitty_img.placements);
        let mut kept = HashMap::new();
        for ((image_id, p), info) in taken.into_iter() {
            if info.virtual_rect.is_some() {
                kept.insert((image_id, p), info);
                continue;
            }
            self.kitty_remove_placement_from_model(image_id, p, info);
        }
        self.kitty_img.placements = kept;
        if delete {
            let referenced: HashSet<u32> = self
                .kitty_img
                .placements
                .keys()
                .map(|(id, _)| *id)
                .collect();
            let ids: Vec<u32> = self.kitty_img.id_to_data.keys().copied().collect();
            for image_id in ids {
                if !referenced.contains(&image_id) {
                    self.kitty_forget_image(image_id);
                }
            }
            self.kitty_img
                .number_to_id
                .retain(|_, id| referenced.contains(id));
        }
        // Dropping every non-virtual placement turns the affected images
        // into prune candidates; check the budget right away so that a
        // client exiting with a "delete all" (eg: pdfterm) releases
        // excess image data without waiting for the next transmission.
        self.kitty_prune_over_budget(None);
    }

    /// A terminal reset clears everything, virtual placements included
    pub(crate) fn kitty_reset_all(&mut self) {
        for ((image_id, p), info) in std::mem::take(&mut self.kitty_img.placements).into_iter() {
            self.kitty_remove_placement_from_model(image_id, p, info);
        }
        self.kitty_img.id_to_data.clear();
        self.kitty_img.id_generation.clear();
        self.kitty_img.used_memory = 0;
        self.kitty_img.number_to_id.clear();
        self.kitty_img.accumulator.clear();
        self.kitty_placeholder_state = None;
        // A reset is an explicit clear: spilled payloads must not
        // survive in the on-disk image cache either
        let spilled = std::mem::take(&mut self.kitty_img.spilled);
        for (hash, _) in spilled.values() {
            wezterm_blob_leases::image_disk_cache::remove(hash);
        }
    }

    /// Whether any row of the placement still exists in the screen or
    /// the scrollback.  Rows form one contiguous range, so once both
    /// end rows have scrolled out of the scrollback the whole
    /// placement is gone from the model.
    fn placement_rows_in_model(screen: &crate::Screen, info: &PlacementInfo) -> bool {
        if info.rows == 0 {
            return false;
        }
        let last = info.first_row + info.rows as StableRowIndex - 1;
        screen.stable_row_to_phys(info.first_row).is_some()
            || screen.stable_row_to_phys(last).is_some()
    }

    /// Propagate the current configuration to the global on-disk image
    /// cache.  Cheap and idempotent, so it is safe to call from the
    /// budget prune and restore paths; config reloads take effect on
    /// the next call without additional plumbing.
    fn configure_kitty_image_disk_cache(&self) {
        wezterm_blob_leases::image_disk_cache::configure(
            self.config.kitty_image_disk_cache_enabled(),
            self.config.kitty_image_disk_cache_budget_bytes(),
            self.config.kitty_image_disk_cache_dir().as_deref(),
        );
    }

    /// Write image_id's payload to the on-disk image cache ahead of
    /// releasing it from memory, so that a later placement of the id
    /// can restore it.  Returns true when the payload is recoverable.
    fn kitty_spill_image_to_disk(&mut self, image_id: u32) -> bool {
        let Some(data) = self.kitty_img.id_to_data.get(&image_id) else {
            return false;
        };
        let hash = data.hash();
        let generation = self
            .kitty_img
            .id_generation
            .get(&image_id)
            .copied()
            .unwrap_or(0);
        if data.spill_to_disk() {
            log::info!("kitty image {image_id} spilled to the image disk cache");
            self.kitty_img.record_spilled(image_id, hash, generation);
            true
        } else {
            false
        }
    }

    /// Look up image data by id, restoring it from the on-disk image
    /// cache when a budget prune spilled it there.
    ///
    /// This is deliberately only reached from placement paths: the
    /// restored image is protected by its new placement, so the next
    /// prune can only release other, unreferenced images.  Never prune
    /// from here; the budget is re-enforced at the existing prune
    /// trigger points (transmission and deletes), which keeps the
    /// restore path free of restore/release cycles.
    fn kitty_img_lookup_or_restore(&mut self, image_id: u32) -> Option<Arc<ImageData>> {
        if let Some(data) = self.kitty_img.id_to_data.get(&image_id) {
            return Some(Arc::clone(data));
        }
        if !self.config.kitty_image_disk_cache_enabled() {
            return None;
        }
        self.configure_kitty_image_disk_cache();
        let (hash, _generation) = *self.kitty_img.spilled.get(&image_id)?;

        // The dedup cache may still hold the pixels under the same
        // content hash; prefer it over touching the disk.
        let data = match self.image_cache.peek(&hash) {
            Some(data) => Some(Arc::clone(data)),
            None => ImageData::load_from_disk(&hash),
        };
        let data = match data {
            Some(data) => data,
            None => {
                // The disk budget evicted the payload (or the spill
                // failed after all); forget the id so that future
                // placements report ENOENT without retrying.
                self.kitty_img.spilled.remove(&image_id);
                wezterm_blob_leases::image_disk_cache::note_restore_miss();
                log::info!(
                    "kitty image {image_id} was spilled to the image disk cache \
                     but is no longer recoverable"
                );
                return None;
            }
        };
        wezterm_blob_leases::image_disk_cache::note_restore_hit();
        log::info!("kitty image {image_id} restored from the image disk cache");
        // Recording fresh data also drops the spilled record
        self.kitty_img
            .record_id_to_data(image_id, Arc::clone(&data));
        Some(data)
    }

    /// Explicitly forget an image: the client deleted it, so its data
    /// must not survive in the on-disk image cache either.
    fn kitty_forget_image(&mut self, image_id: u32) {
        self.kitty_img.remove_data_for_id(image_id);
        if let Some((hash, _)) = self.kitty_img.spilled.remove(&image_id) {
            wezterm_blob_leases::image_disk_cache::remove(&hash);
        }
    }

    /// Enforce the configured kitty graphics storage budget, following
    /// the spec's storage quota recommendation: when the retained image
    /// data exceeds the budget, release the oldest images that no
    /// placement still references; clients then observe ENOENT if they
    /// reference a released id.
    ///
    /// When the on-disk image cache is enabled, released images are
    /// spilled to it first, and a later placement of a released id
    /// restores the payload instead of observing ENOENT; see
    /// [`Self::kitty_img_lookup_or_restore`].
    ///
    /// Images whose placements still intersect the screen or scrollback
    /// are never released, so a well-behaved client never loses anything
    /// it can currently see.  `keep` spares an image that is about to be
    /// placed (the transmit-and-display flow prunes between storing the
    /// image and creating its placement).
    ///
    /// Placement records whose rows have scrolled out of the scrollback
    /// no longer correspond to any cells; they are dropped here so that
    /// they cannot pin their images (and the placement table) forever.
    fn kitty_prune_over_budget(&mut self, keep: Option<u32>) {
        let stale: Vec<(u32, Option<u32>)> = self
            .kitty_img
            .placements
            .iter()
            .filter(|(_, info)| {
                info.virtual_rect.is_none() && !Self::placement_rows_in_model(self.screen(), info)
            })
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.kitty_img.placements.remove(&key);
        }

        // Disk read-backs happen wherever pixels are rendered, outside
        // of the incremental accounting below; recompute the total so
        // that the budget decision always sees reality.
        self.kitty_img.used_memory = self
            .kitty_img
            .id_to_data
            .values()
            .map(|data| data.len())
            .sum();

        let budget = self.config.kitty_image_storage_budget_mb() as usize * 1024 * 1024;
        if self.kitty_img.used_memory <= budget {
            return;
        }

        // The disk cache is only ever consulted as the overflow
        // destination of this budget; when it is disabled, released
        // images are simply dropped as before.
        let spill_to_disk = self.config.kitty_image_disk_cache_enabled();
        if spill_to_disk {
            self.configure_kitty_image_disk_cache();
        }

        let referenced: HashSet<u32> = self
            .kitty_img
            .placements
            .keys()
            .map(|(id, _)| *id)
            .collect();
        let mut candidates: Vec<(u64, u32)> = self
            .kitty_img
            .id_to_data
            .keys()
            .filter(|id| Some(**id) != keep && !referenced.contains(*id))
            .map(|id| {
                (
                    self.kitty_img.id_generation.get(id).copied().unwrap_or(0),
                    *id,
                )
            })
            .collect();
        candidates.sort_unstable();

        let mut released = 0;
        let mut spilled = 0;
        let before = self.kitty_img.used_memory;
        for (_, image_id) in candidates {
            if self.kitty_img.used_memory <= budget {
                break;
            }
            released += 1;
            if spill_to_disk && self.kitty_spill_image_to_disk(image_id) {
                spilled += 1;
                // Keep the number→id mapping so that a placement by
                // image_number can also restore from the disk cache.
            } else {
                self.kitty_img
                    .number_to_id
                    .retain(|_, mapped| *mapped != image_id);
            }
            self.kitty_img.remove_data_for_id(image_id);
        }
        if released > 0 {
            log::info!(
                "kitty image storage {} exceeded budget {}; released {} image(s), \
                 {} of them spilled to the image disk cache, now using {}",
                before,
                budget,
                released,
                spilled,
                self.kitty_img.used_memory
            );
        }

        // Everything still stored is referenced (or was just kept).
        // Rather than dropping images a client can currently see, swap
        // their pixel payloads out to the disk cache; rendering reads
        // pixels back on demand, so nothing the client displays is
        // lost.  Candidates are ordered so that images never read back
        // since their last swap go first (oldest transmission first),
        // while a read-back — the image was displayed again — always
        // defers the swap behind those, most recent read-back last.
        // Viewing an image therefore never ping-pongs it straight
        // back out to disk.
        if spill_to_disk && self.kitty_img.used_memory > budget {
            let mut candidates: Vec<(u64, u64, u32)> = self
                .kitty_img
                .id_to_data
                .keys()
                .filter(|id| Some(**id) != keep)
                .map(|id| {
                    let generation = self.kitty_img.id_generation.get(id).copied().unwrap_or(0);
                    let restored = self
                        .kitty_img
                        .id_to_data
                        .get(id)
                        .map(|data| data.restored_tick())
                        .unwrap_or(0);
                    // Images never read back since their last swap are
                    // the coldest, ordered by transmission age; a
                    // read-back (ie: the image was displayed again)
                    // always ranks after those, most recent last
                    (restored, generation, *id)
                })
                .collect();
            candidates.sort_unstable();

            let mut swapped = 0;
            for (_, _, image_id) in candidates {
                if self.kitty_img.used_memory <= budget {
                    break;
                }
                let Some(data) = self.kitty_img.id_to_data.get(&image_id) else {
                    continue;
                };
                let before = data.len();
                if data.swap_out_to_disk() {
                    // The marker reports zero footprint
                    self.kitty_img.used_memory -= before;
                    swapped += 1;
                }
            }
            if swapped > 0 {
                log::info!(
                    "kitty image storage still over budget after releases; swapped {} \
                     referenced image(s) to the image disk cache, now using {}",
                    swapped,
                    self.kitty_img.used_memory
                );
            }
        }
    }

    /// Handle a kitty graphics protocol delete command.
    /// The lowercase forms remove the matching placements but keep the
    /// image data; the uppercase forms additionally free the data of any
    /// image that no longer has any placements.
    fn kitty_img_delete(&mut self, what: KittyImageDelete, verbosity: KittyImageVerbosity) {
        match what {
            KittyImageDelete::ByImageId {
                image_id,
                placement_id,
                delete,
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
                    self.kitty_forget_image(image_id);
                }
            }
            KittyImageDelete::All { delete } => {
                self.kitty_remove_all_placements(delete);
            }
            KittyImageDelete::ByImageNumber {
                image_number,
                placement_id,
                delete,
            } => match self.kitty_img.number_to_id.get(&image_number).copied() {
                Some(image_id) => {
                    self.kitty_remove_placement(image_id, placement_id);
                    if delete {
                        self.kitty_forget_image(image_id);
                    }
                }
                None => {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        None,
                        Some(image_number),
                        None,
                        "ENOENT:no image with that number".to_string(),
                    );
                }
            },
            KittyImageDelete::ByIdRange { from, to, delete } => {
                self.kitty_delete_placements_where(delete, |image_id, _, _| {
                    image_id >= from && image_id <= to
                });
            }
            KittyImageDelete::AtCursorPosition { delete } => {
                let (col, row) = self.cursor_cell();
                self.kitty_delete_placements_where(delete, |_, _, info| {
                    placement_intersects_cell(info, col, row)
                });
            }
            KittyImageDelete::DeleteAt { x, y, delete } => {
                let col = x as usize;
                let row = self.screen().visible_row_to_stable_row(y as i64);
                self.kitty_delete_placements_where(delete, |_, _, info| {
                    placement_intersects_cell(info, col, row)
                });
            }
            KittyImageDelete::DeleteAtZ { x, y, z, delete } => {
                let col = x as usize;
                let row = self.screen().visible_row_to_stable_row(y as i64);
                self.kitty_delete_placements_where(delete, |_, _, info| {
                    info.z_index == z && placement_intersects_cell(info, col, row)
                });
            }
            KittyImageDelete::DeleteColumn { x, delete } => {
                let col = x as usize;
                self.kitty_delete_placements_where(delete, |_, _, info| {
                    col >= info.first_col && col < info.first_col + info.cols
                });
            }
            KittyImageDelete::DeleteRow { y, delete } => {
                let row = self.screen().visible_row_to_stable_row(y as i64);
                self.kitty_delete_placements_where(delete, |_, _, info| {
                    row >= info.first_row && row < info.first_row + info.rows as StableRowIndex
                });
            }
            KittyImageDelete::DeleteZ { z, delete } => {
                self.kitty_delete_placements_where(delete, |_, _, info| info.z_index == z);
            }
            KittyImageDelete::AnimationFrames {
                image_id,
                image_number,
                frame_number,
                delete,
            } => {
                self.kitty_delete_frames(image_id, image_number, frame_number, delete, verbosity);
            }
        }

        // Deleting placements can orphan image data; enforce the storage
        // budget now rather than at the next transmission.
        self.kitty_prune_over_budget(None);
    }

    /// d=f: delete animation frames. The 1-based frame number comes
    /// from c= (frame 1, the root frame, by default), following kitty.
    fn kitty_delete_frames(
        &mut self,
        image_id: Option<u32>,
        image_number: Option<u32>,
        frame_number: Option<u32>,
        delete: bool,
        verbosity: KittyImageVerbosity,
    ) {
        let image_id = match image_id
            .or_else(|| image_number.and_then(|no| self.kitty_img.number_to_id.get(&no).copied()))
        {
            Some(id) => id,
            None => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    image_id,
                    image_number,
                    None,
                    "ENOENT:no such image".to_string(),
                );
                return;
            }
        };

        let anim = match self.kitty_img.id_to_data.get(&image_id) {
            Some(anim) => Arc::clone(anim),
            None => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    None,
                    "ENOENT:no such image".to_string(),
                );
                return;
            }
        };

        let mut anim = anim.data();
        // Set when the uppercase form removes a still image entirely
        let mut remove_still = false;
        match &mut *anim {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    None,
                    "EINVAL:cannot delete frames of an encoded image".to_string(),
                );
                return;
            }
            ImageDataType::DiskBacked { .. } => {
                // data() reads swapped pixels back, so this arm is not
                // reachable in practice
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    None,
                    "EINVAL:cannot delete frames of a swapped image".to_string(),
                );
                return;
            }
            ImageDataType::Rgba8 { .. } => {
                // A still image has no animation frames; the uppercase
                // form removes the image itself, like kitty does
                if delete {
                    remove_still = true;
                }
            }
            ImageDataType::AnimRgba8 {
                frames,
                durations,
                hashes,
                ..
            } => {
                let frame = frame_number.unwrap_or(1);
                if frame == 0 || frame as usize > frames.len() {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        Some(image_id),
                        image_number,
                        None,
                        format!("EINVAL:frame {} is out of range 1-{}", frame, frames.len()),
                    );
                    return;
                }
                let idx = frame as usize - 1;
                frames.remove(idx);
                durations.remove(idx);
                hashes.remove(idx);
            }
        }
        drop(anim);
        if remove_still {
            self.kitty_remove_placement(image_id, None);
            self.kitty_forget_image(image_id);
            return;
        }
        self.kitty_touch_image_placements(image_id);
    }

    /// a=a: control the playback of an animated image.
    fn kitty_animate(&mut self, cmd: KittyImageAnimate) -> anyhow::Result<()> {
        let image_id = match cmd.image_id.or_else(|| {
            cmd.image_number
                .and_then(|no| self.kitty_img.number_to_id.get(&no).copied())
        }) {
            Some(id) => id,
            None => {
                return Err(KittyProtocolError {
                    code: "ENOENT",
                    detail: "no such image".to_string(),
                }
                .into())
            }
        };

        let anim = self
            .kitty_img
            .id_to_data
            .get(&image_id)
            .ok_or_else(|| {
                anyhow::Error::new(KittyProtocolError {
                    code: "ENOENT",
                    detail: format!("no such image id {}", image_id),
                })
            })?
            .clone();

        let mut anim = anim.data();
        let frames_len = match &*anim {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                return Err(KittyProtocolError {
                    code: "EINVAL",
                    detail: "cannot control an encoded image".to_string(),
                }
                .into())
            }
            ImageDataType::Rgba8 { .. } => 1,
            ImageDataType::AnimRgba8 { frames, .. } => frames.len(),
            // data() reads swapped pixels back, so this is unreachable
            // in practice; without pixels there are no frames to name
            ImageDataType::DiskBacked { frame_count, .. } => *frame_count as usize,
        };

        // Validate the frame references up front so that the command
        // either applies completely or answers EINVAL
        if let Some(f) = cmd.frame_number {
            if f == 0 || f as usize > frames_len {
                return Err(KittyProtocolError {
                    code: "EINVAL",
                    detail: format!("frame {} is out of range 1-{}", f, frames_len),
                }
                .into());
            }
        }
        if let Some(r) = cmd.gap_frame {
            if r == 0 || r as usize > frames_len {
                return Err(KittyProtocolError {
                    code: "EINVAL",
                    detail: format!("frame {} is out of range 1-{}", r, frames_len),
                }
                .into());
            }
        }

        // The gap applies to the frame selected by r=, else by c=,
        // else to the root frame
        let gap_frame = cmd.gap_frame.or(cmd.frame_number).unwrap_or(1);

        match &mut *anim {
            ImageDataType::AnimRgba8 {
                durations, control, ..
            } => {
                if let Some(state) = cmd.state {
                    match state {
                        KittyAnimationState::Stop => {
                            control.running = false;
                            control.loading = false;
                            // Stopping the animation resets the loop counter
                            control.loops_done = 0;
                        }
                        KittyAnimationState::Loading => {
                            control.running = true;
                            control.loading = true;
                        }
                        KittyAnimationState::Running => {
                            control.running = true;
                            control.loading = false;
                        }
                    }
                }
                if let Some(v) = cmd.loop_count {
                    // v=1 loops forever; any other positive value loops
                    // v-1 times before the animation stops
                    control.loop_count = Some(v);
                    control.loops_done = 0;
                }
                if let Some(f) = cmd.frame_number {
                    control.paused_frame = Some(f);
                }
                if let Some(gap) = cmd.gap {
                    // Negative gaps are clamped to zero (gapless), as kitty does
                    durations[gap_frame as usize - 1] = Duration::from_millis(gap.max(0) as u64);
                }
            }
            // A still image: only the frame selection is meaningful and
            // there is nothing to seek in a single frame image
            ImageDataType::Rgba8 { .. } => {}
            // data() reads swapped pixels back, so unreachable
            ImageDataType::DiskBacked { .. } => {}
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => unreachable!(),
        }
        drop(anim);
        self.kitty_touch_image_placements(image_id);
        Ok(())
    }

    /// Render the image slice referenced by a printed unicode
    /// placeholder cell (a U+10EEEE grapheme).
    ///
    /// The lower 24 bits of the image id come from the foreground
    /// color, the placement id from the underline color, and the
    /// row/column (plus the high 8 bits of the id) from combining
    /// diacritics following the placeholder character. Successive
    /// compatible cells form a run whose unknown values are inherited
    /// from the cell to the left, with the column implicitly
    /// incrementing, as the protocol specifies.
    pub(crate) fn kitty_render_placeholder(
        &mut self,
        grapheme: &str,
        pen: &CellAttributes,
        x: usize,
        y: i64,
    ) {
        if !self.config.enable_kitty_graphics() {
            return;
        }
        let mut chars = grapheme.chars();
        if chars.next() != Some(IMAGE_PLACEHOLDER_CHAR) {
            return;
        }

        // The first diacritic encodes the row, the second the column,
        // the third the high 8 bits of the image id. A missing or
        // zero-valued diacritic means "unknown".
        let mut marks = [0u32; 3];
        for (slot, mark) in marks.iter_mut().zip(chars.by_ref()) {
            *slot = diacritic_to_num(mark).unwrap_or(0);
        }
        let [row0, col0, hi0] = marks;

        let id_lower = placeholder_color_to_id(pen.foreground());
        if id_lower == 0 {
            // The default foreground cannot reference an image
            self.kitty_placeholder_state = None;
            return;
        }
        let pid = placeholder_color_to_id(pen.underline_color());

        let state = self.kitty_placeholder_state.take();
        let (row, col, hi8) = match state {
            Some(s)
                if s.y == y
                    && s.id_lower == id_lower
                    && s.pid == pid
                    && (row0 == 0 || row0 == s.row)
                    && (col0 == 0 || col0 == s.col + 1)
                    && (hi0 == 0 || hi0 == s.hi8) =>
            {
                // This cell continues the run to its left
                (s.row, s.col + 1, s.hi8)
            }
            _ => (row0.max(1), col0.max(1), hi0.max(1)),
        };
        let run = KittyPlaceholderRun {
            y,
            id_lower,
            pid,
            row,
            col,
            hi8,
        };
        let image_id = run.image_id();
        self.kitty_placeholder_state = Some(run);

        // Find the virtual placement the placeholder text refers to;
        // with no explicit placement id any virtual placement of the
        // image will do
        let found = self
            .kitty_img
            .placements
            .iter()
            .find(|((id, pid_key), info)| {
                *id == image_id
                    && info.virtual_rect.is_some()
                    && (pid == 0 || *pid_key == Some(pid))
            });
        // Copy out what is needed before the potential disk-cache
        // restore below mutates the placement table's owner
        let (vpid, cols_total, rows_total, z_index) = match found {
            Some(((_vid, vpid), info)) => match info.virtual_rect {
                Some(rect) => (*vpid, rect.0, rect.1, info.z_index),
                None => return,
            },
            None => return,
        };
        let data = match self.kitty_img_lookup_or_restore(image_id) {
            Some(data) => data,
            None => return,
        };

        self.kitty_attach_placeholder_slice(
            &data, cols_total, rows_total, row, col, x, y, z_index, image_id, vpid,
        );
    }

    /// Attach the (row, col) cell of a cols_total x rows_total scaled
    /// image to the screen cell at (x, y).
    fn kitty_attach_placeholder_slice(
        &mut self,
        data: &Arc<ImageData>,
        cols_total: usize,
        rows_total: usize,
        row: u32,
        col: u32,
        x: usize,
        y: i64,
        z_index: i32,
        image_id: u32,
        placement_id: Option<u32>,
    ) {
        if row as usize > rows_total || col as usize > cols_total {
            return;
        }
        let screen = self.screen();
        if x >= screen.physical_cols || y < 0 || y as usize >= screen.physical_rows {
            return;
        }
        let seqno = self.seqno;

        let top_left = TextureCoordinate::new(
            NotNan::new((col - 1) as f32 / cols_total as f32).unwrap_or_else(|_| unreachable!()),
            NotNan::new((row - 1) as f32 / rows_total as f32).unwrap_or_else(|_| unreachable!()),
        );
        let bottom_right = TextureCoordinate::new(
            NotNan::new(col as f32 / cols_total as f32).unwrap_or_else(|_| unreachable!()),
            NotNan::new(row as f32 / rows_total as f32).unwrap_or_else(|_| unreachable!()),
        );

        let mut cell = self
            .screen_mut()
            .get_cell(x, y)
            .cloned()
            .unwrap_or_else(Cell::blank);
        let img = Box::new(ImageCell::with_z_index(
            top_left,
            bottom_right,
            Arc::clone(data),
            z_index,
            0,
            0,
            0,
            0,
            Some(image_id),
            placement_id,
        ));
        cell.attrs_mut().attach_image(img);
        self.screen_mut().set_cell(x, y, &cell, seqno);
    }

    /// Bump the last-change seqno of the lines covered by the image's
    /// placements, so that the renderer picks up animation control and
    /// frame changes that do not otherwise touch the screen model.
    fn kitty_touch_image_placements(&mut self, image_id: u32) {
        let seqno = self.seqno;
        let infos: Vec<PlacementInfo> = self
            .kitty_img
            .placements
            .iter()
            .filter(|((id, _), _)| *id == image_id)
            .map(|(_, info)| *info)
            .collect();
        for info in infos {
            let screen = self.screen_mut();
            let range = screen
                .stable_range(&(info.first_row..info.first_row + info.rows as StableRowIndex));
            for idx in range {
                screen.line_mut(idx).update_last_change_seqno(seqno);
            }
        }
    }

    /// The cursor position expressed as a (viewport column, stable row)
    /// pair, suitable for intersection tests against placements.
    fn cursor_cell(&self) -> (usize, StableRowIndex) {
        (
            self.cursor.x,
            self.screen().visible_row_to_stable_row(self.cursor.y),
        )
    }

    /// Remove every placement for which `pred` returns true.
    /// When `delete_data` is set (the uppercase delete forms), also free
    /// the image data of the affected images that no longer have any
    /// placements, matching the spec's managed-group semantics.
    fn kitty_delete_placements_where(
        &mut self,
        delete_data: bool,
        pred: impl Fn(u32, Option<u32>, &PlacementInfo) -> bool,
    ) {
        let matching: Vec<(u32, Option<u32>, PlacementInfo)> = self
            .kitty_img
            .placements
            .iter()
            .filter(|(key, info)| pred(key.0, key.1, info))
            .map(|(key, info)| (key.0, key.1, *info))
            .collect();

        let mut touched: HashSet<u32> = HashSet::new();
        for (image_id, placement_id, info) in matching {
            touched.insert(image_id);
            self.kitty_img.placements.remove(&(image_id, placement_id));
            self.kitty_remove_placement_from_model(image_id, placement_id, info);
        }

        if delete_data {
            let still_referenced: HashSet<u32> = self
                .kitty_img
                .placements
                .keys()
                .map(|(id, _)| *id)
                .collect();
            for image_id in &touched {
                if !still_referenced.contains(image_id) {
                    self.kitty_forget_image(*image_id);
                }
            }
        }
    }

    fn kitty_send_response(
        &mut self,
        verbosity: KittyImageVerbosity,
        success: bool,
        image_id: Option<u32>,
        image_no: Option<u32>,
        placement_id: Option<u32>,
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

        // The spec response form is <ESC>_Gi=<id>[,I=<num>][,p=<placement
        // id>];OK or error<ESC>\ with the keys echoed back only when the
        // client supplied them.
        let mut keys = vec![];
        if let Some(id) = image_id {
            keys.push(format!("i={}", id));
        }
        if let Some(no) = image_no {
            keys.push(format!("I={}", no));
        }
        if let (Some(id), Some(p)) = (image_id, placement_id) {
            // Placement ids are only meaningful for a non-zero image id
            if id != 0 {
                keys.push(format!("p={}", p));
            }
        }
        if keys.is_empty() {
            write!(self.writer, "\x1b_G{}\x1b\\", message).ok();
        } else {
            write!(self.writer, "\x1b_G{};{}\x1b\\", keys.join(","), message).ok();
        }
        self.writer.flush().ok();
    }

    /// Report a failed kitty graphics command back to the client.
    /// Errors that carry one of the protocol error codes keep it;
    /// anything else is treated as invalid input, matching how kitty
    /// classifies malformed control data.
    fn kitty_reply_error(
        &mut self,
        verbosity: KittyImageVerbosity,
        image_id: Option<u32>,
        image_no: Option<u32>,
        placement_id: Option<u32>,
        err: &anyhow::Error,
    ) {
        let message = match err.downcast_ref::<KittyProtocolError>() {
            Some(e) => e.to_string(),
            None => format!("EINVAL:{:#}", err),
        };
        self.kitty_send_response(verbosity, false, image_id, image_no, placement_id, message);
    }

    fn kitty_frame_compose(
        &mut self,
        frame: KittyImageFrameCompose,
        _verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let image_id = match frame.image_number {
            Some(no) => match self.kitty_img.number_to_id.get(&no) {
                Some(id) => *id,
                None => {
                    return Err(KittyProtocolError {
                        code: "ENOENT",
                        detail: format!("no such image_number {}", no),
                    }
                    .into())
                }
            },
            None => frame.image_id.ok_or_else(|| {
                anyhow::Error::new(KittyProtocolError {
                    code: "EINVAL",
                    detail: "no image_id or image_number".to_string(),
                })
            })?,
        };

        let src_frame = frame.source_frame.ok_or_else(|| {
            anyhow::Error::new(KittyProtocolError {
                code: "EINVAL",
                detail: "missing source frame".to_string(),
            })
        })? as usize;
        let target_frame = frame.target_frame.ok_or_else(|| {
            anyhow::Error::new(KittyProtocolError {
                code: "EINVAL",
                detail: "missing target frame".to_string(),
            })
        })? as usize;

        let img = self.kitty_img_lookup_or_restore(image_id).ok_or_else(|| {
            anyhow::Error::new(KittyProtocolError {
                code: "ENOENT",
                detail: format!("invalid image id {}", image_id),
            })
        })?;

        let mut img = img.data();
        match &mut *img {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("invalid image type")
            }
            // data() reads swapped pixels back, so unreachable
            ImageDataType::DiskBacked { .. } => anyhow::bail!("invalid image type"),
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
        _verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<u32> {
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
                return Err(KittyProtocolError {
                    code: "ENOENT",
                    detail: format!(
                        "no matching image id {} in id_to_data for image_number {:?}",
                        image_id, image_number
                    ),
                }
                .into())
            }
        };

        let mut anim = anim.data();
        let x = frame.x.unwrap_or(0);
        let y = frame.y.unwrap_or(0);
        let frame_gap = Duration::from_millis(match frame.duration_ms {
            None | Some(0) => 40,
            // A negative gap denotes a gapless frame: it is stored
            // with a zero duration and skipped during playback.
            Some(n) if n < 0 => 0,
            Some(n) => n as u64,
        });

        match &mut *anim {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("Expected decoded image for image id {}", image_id)
            }
            // data() reads swapped pixels back, so unreachable
            ImageDataType::DiskBacked { .. } => {
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
                            control: Default::default(),
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
                ..
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

        Ok(image_id)
    }

    fn kitty_img_transmit_inner(
        &mut self,
        transmit: KittyImageTransmit,
    ) -> anyhow::Result<(u32, Option<u32>, ImageDataType)> {
        log::trace!("transmit {:?}", transmit);
        let (id, no) = match (transmit.image_id, transmit.image_number) {
            (Some(_), Some(_)) => {
                return Err(KittyProtocolError {
                    code: "EINVAL",
                    detail: "cannot use both i= and I= in the same request".to_string(),
                }
                .into())
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

        // Failures to read from a file or shared memory are reported as
        // EBADF; problems with the payload itself are EINVAL.
        let from_storage = matches!(
            transmit.data,
            KittyImageData::File { .. }
                | KittyImageData::TemporaryFile { .. }
                | KittyImageData::SharedMem { .. }
        );
        let data = transmit.data.load_data().map_err(|err| {
            let code = if from_storage { "EBADF" } else { "EINVAL" };
            anyhow::Error::new(KittyProtocolError {
                code,
                detail: format!("{:#}", err),
            })
        })?;

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
        self.kitty_prune_over_budget(Some(image_id));

        if image_number.is_some() {
            self.kitty_send_response(
                verbosity,
                true,
                Some(image_id),
                image_number,
                None,
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

/// Whether a placement covers the given viewport cell, identified by
/// its column and stable row.
fn placement_intersects_cell(info: &PlacementInfo, col: usize, row: StableRowIndex) -> bool {
    row >= info.first_row
        && row < info.first_row + info.rows as StableRowIndex
        && col >= info.first_col
        && col < info.first_col + info.cols
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

/// Interpret a cell color as the (lower 24 bits of an) image id, per
/// the unicode placeholder scheme: a palette index is an 8-bit id and
/// a truecolor value is a 24-bit id. The default color cannot
/// reference an image and yields 0.
fn placeholder_color_to_id(color: crate::color::ColorAttribute) -> u32 {
    use wezterm_cell::color::ColorAttribute;
    match color {
        ColorAttribute::TrueColorWithPaletteFallback(rgba, _)
        | ColorAttribute::TrueColorWithDefaultFallback(rgba) => {
            ((rgba.0 * 255.) as u32) << 16 | ((rgba.1 * 255.) as u32) << 8 | (rgba.2 * 255.) as u32
        }
        ColorAttribute::PaletteIndex(p) => p as u32,
        ColorAttribute::Default => 0,
    }
}
