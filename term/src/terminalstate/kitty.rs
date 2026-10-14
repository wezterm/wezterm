use crate::terminalstate::image::*;
use crate::terminalstate::{ImageAttachParams, PlacementInfo};
use crate::{StableRowIndex, TerminalState};
use ::image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, RgbImage, Rgba, RgbaImage,
};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use wezterm_cell::image::ImageDataType;
use wezterm_escape_parser::apc::{
    KittyAnimationState, KittyFrameCompositionMode, KittyImage, KittyImageAnimate,
    KittyImageCompression, KittyImageData, KittyImageDelete, KittyImageFormat, KittyImageFrame,
    KittyImageFrameCompose, KittyImagePlacement, KittyImageTransmit, KittyImageVerbosity,
};
use wezterm_surface::change::ImageData;

#[derive(Debug, Default)]
pub struct KittyImageState {
    accumulator: Vec<KittyImage>,
    max_image_id: u32,
    number_to_id: HashMap<u32, u32>,
    id_to_data: HashMap<u32, Arc<ImageData>>,
    placements: HashMap<(u32, Option<u32>), PlacementInfo>,
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
            let referenced: HashSet<u32> = self.placements.keys().map(|(k, _)| *k).collect();
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
        if image_id != 0 {
            self.kitty_remove_placement(image_id, placement.placement_id);
        }
        let img = Arc::clone(self.kitty_img.id_to_data.get(&image_id).ok_or_else(|| {
            anyhow::Error::new(KittyProtocolError {
                code: "ENOENT",
                detail: format!(
                    "no matching image id {} in id_to_data for image_number {:?}",
                    image_id, image_number
                ),
            })
        })?);

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

    fn kitty_remove_placement(&mut self, image_id: u32, placement_id: Option<u32>) {
        if placement_id.is_some() {
            if let Some(info) = self.kitty_img.placements.remove(&(image_id, placement_id)) {
                log::trace!("removed placement {} {:?}", image_id, placement_id);
                self.kitty_remove_placement_from_model(image_id, placement_id, info);
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
        if delete {
            self.kitty_img.id_to_data.clear();
            self.kitty_img.used_memory = 0;
            self.kitty_img.number_to_id.clear();
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
                    self.kitty_img.remove_data_for_id(image_id);
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
                        self.kitty_img.remove_data_for_id(image_id);
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
            self.kitty_img.remove_data_for_id(image_id);
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
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => unreachable!(),
        }
        drop(anim);
        self.kitty_touch_image_placements(image_id);
        Ok(())
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
                    self.kitty_img.remove_data_for_id(*image_id);
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

        let img = self.kitty_img.id_to_data.get(&image_id).ok_or_else(|| {
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
