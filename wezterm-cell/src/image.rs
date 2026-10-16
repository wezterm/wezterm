//! Images.
//! This module has some helpers for modeling terminal cells that are filled
//! with image data.
//! We're targeting the iTerm image protocol initially, with sixel as an obvious
//! follow up.
//! Kitty has an extensive and complex graphics protocol
//! whose docs are here:
//! <https://github.com/kovidgoyal/kitty/blob/master/docs/graphics-protocol.rst>
//! Both iTerm2 and Sixel appear to have semantics that allow replacing the
//! contents of a single chararcter cell with image data, whereas the kitty
//! protocol appears to track the images out of band as attachments with
//! z-order.

use ordered_float::NotNan;
#[cfg(feature = "use_serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
#[cfg(feature = "std")]
use wezterm_blob_leases::{BlobLease, BlobManager};

#[cfg(feature = "use_serde")]
fn deserialize_notnan<'de, D>(deserializer: D) -> Result<NotNan<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f32::deserialize(deserializer)?;
    NotNan::new(value).map_err(|e| serde::de::Error::custom(format!("{:?}", e)))
}

#[cfg(feature = "use_serde")]
#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_notnan<S>(value: &NotNan<f32>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.into_inner().serialize(serializer)
}

/// A 2D point with floating coordinates, that guarantees finite (non-NaN) values.
///
/// The coordinates are intended to be normalized to [0,1] per axis, with (0,0)
/// and (1,1) as opposite corners, so a pair of points can delimit a sub-region
/// of a texture or image.
/// The range is a convention of the caller, the type itself does not enforce it.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureCoordinate {
    #[cfg_attr(
        feature = "use_serde",
        serde(
            deserialize_with = "deserialize_notnan",
            serialize_with = "serialize_notnan"
        )
    )]
    pub x: NotNan<f32>,
    #[cfg_attr(
        feature = "use_serde",
        serde(
            deserialize_with = "deserialize_notnan",
            serialize_with = "serialize_notnan"
        )
    )]
    pub y: NotNan<f32>,
}

impl TextureCoordinate {
    pub fn new(x: NotNan<f32>, y: NotNan<f32>) -> Self {
        Self { x, y }
    }

    pub fn new_f32(x: f32, y: f32) -> Self {
        let x = NotNan::new(x).unwrap();
        let y = NotNan::new(y).unwrap();
        Self::new(x, y)
    }
}

/// Tracks data for displaying an image (or slice of one) in the place of the
/// normal cell character data. Since an Image can span multiple cells, we need
/// to logically slice up the image and track each slice of it.
///
/// Each cell knows its "texture coordinates" within that image so we can later
/// render the right slice for the cell.
///
/// This models the Kitty graphics protocol, where an image placement can span
/// several cells: the placement is sliced into one [`ImageCell`] per covered
/// cell, all referencing the same [`ImageData`], `image_id`, `placement_id`.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageCell {
    /// Floating texture coordinate for the top left of this cell.
    /// (0,0) is the top left of the ImageData.
    /// (1, 1) is the bottom right.
    top_left: TextureCoordinate,
    /// Floating texture coordinates for the bottom right of this cell.
    bottom_right: TextureCoordinate,
    /// References the underlying image data
    data: Arc<ImageData>,
    z_index: i32,
    /// When rendering in the cell, use this offset from the top left
    /// of the cell
    padding_left: u16,
    padding_top: u16,
    padding_right: u16,
    padding_bottom: u16,

    /// For the kitty protocol: the image id this cell's slice belongs to.
    /// Must be in sync with the attached [`ImageData`].
    image_id: Option<u32>,
    /// For the kitty protocol: the placement id for this cell.
    /// Must be in sync with the attached image id/data.
    placement_id: Option<u32>,
}

impl ImageCell {
    pub fn new(
        top_left: TextureCoordinate,
        bottom_right: TextureCoordinate,
        data: Arc<ImageData>,
    ) -> Self {
        Self::with_z_index(top_left, bottom_right, data, 0, 0, 0, 0, 0, None, None)
    }

    /// Computes a hash over the image cell that only changes based on its content.
    ///
    /// The hash covers the layout and the image *identity*, not the pixels,
    /// so it is unaffected by eventual in-place edits to the image data.
    pub fn compute_shape_hash<H: Hasher>(&self, hasher: &mut H) {
        self.top_left.hash(hasher);
        self.bottom_right.hash(hasher);
        self.data.hash.hash(hasher);
        self.z_index.hash(hasher);
        self.padding_left.hash(hasher);
        self.padding_top.hash(hasher);
        self.padding_right.hash(hasher);
        self.padding_bottom.hash(hasher);
        self.image_id.hash(hasher);
        self.placement_id.hash(hasher);
    }

    pub fn with_z_index(
        top_left: TextureCoordinate,
        bottom_right: TextureCoordinate,
        data: Arc<ImageData>,
        z_index: i32,
        padding_left: u16,
        padding_top: u16,
        padding_right: u16,
        padding_bottom: u16,
        image_id: Option<u32>,
        placement_id: Option<u32>,
    ) -> Self {
        Self {
            top_left,
            bottom_right,
            data,
            z_index,
            padding_left,
            padding_top,
            padding_right,
            padding_bottom,
            image_id,
            placement_id,
        }
    }

    pub fn matches_placement(&self, image_id: u32, placement_id: Option<u32>) -> bool {
        self.image_id == Some(image_id) && self.placement_id == placement_id
    }

    pub fn has_placement_id(&self) -> bool {
        self.placement_id.is_some()
    }

    pub fn image_id(&self) -> Option<u32> {
        self.image_id
    }

    pub fn placement_id(&self) -> Option<u32> {
        self.placement_id
    }

    pub fn top_left(&self) -> TextureCoordinate {
        self.top_left
    }

    pub fn bottom_right(&self) -> TextureCoordinate {
        self.bottom_right
    }

    pub fn image_data(&self) -> &Arc<ImageData> {
        &self.data
    }

    /// negative z_index is rendered beneath the text layer.
    /// >= 0 is rendered above the text.
    /// negative z_index < INT32_MIN/2 will be drawn under cells
    /// with non-default background colors
    pub fn z_index(&self) -> i32 {
        self.z_index
    }

    /// Returns padding (left, top, right, bottom)
    pub fn padding(&self) -> (u16, u16, u16, u16) {
        (
            self.padding_left,
            self.padding_top,
            self.padding_right,
            self.padding_bottom,
        )
    }
}

/// The raw payload of an image in one of several storage forms:
/// - encoded file bytes (in memory or on-disk in the blob manager)
/// - decoded RGBA pixels (single-frame or animated)
///
/// This is a value type, freely constructed and transformed independent of sharing.
///
/// NOTE: The decoded RGBA variants cache a sha256 hash of their frame(s);
/// mutation through [`ImageData::data`] must refresh them.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Clone, PartialEq, Eq)]
pub enum ImageDataType {
    /// Data is in the native image file format
    /// (best for file formats that have animated content)
    EncodedFile(Vec<u8>),
    /// Data is in the native image file format,
    /// (best for file formats that have animated content)
    /// and is stored as a blob via the blob manager.
    #[cfg(feature = "std")]
    EncodedLease(
        #[cfg_attr(
            feature = "use_serde",
            serde(with = "wezterm_blob_leases::lease_bytes")
        )]
        BlobLease,
    ),
    /// Data is RGBA u8 data
    Rgba8 {
        data: Vec<u8>,
        width: u32,
        height: u32,
        /// sha256 of `data`.
        /// Must be updated whenever `data` changes.
        hash: [u8; 32],
    },
    /// Data is an animated sequence
    AnimRgba8 {
        width: u32,
        height: u32,
        durations: Vec<Duration>,
        frames: Vec<Vec<u8>>,
        /// sha256 of each `frames`.
        /// Must be updated whenever a frame changes.
        hashes: Vec<[u8; 32]>,
        /// Runtime playback state, manipulated by the kitty graphics
        /// protocol a=a animation control command.
        /// Defaults to playing forever, matching the behavior of
        /// animations before this field existed.
        #[cfg_attr(feature = "use_serde", serde(default))]
        control: AnimationControl,
    },
    /// A pixel payload (static or animated) that was swapped out to
    /// the image disk cache to reduce the memory footprint; the hash
    /// is the cache key the payload is stored under.  Cells and
    /// placements keep referencing the image as-is: [`ImageData::data`]
    /// reads the pixels back on demand.
    DiskBacked {
        /// Content hash of the swapped payload; the disk cache key
        hash: [u8; 32],
        width: u32,
        height: u32,
        /// Frame count of the swapped payload (1 for static images)
        frame_count: u32,
    },
}

/// Runtime animation control state for an [`ImageDataType::AnimRgba8`],
/// manipulated via the kitty graphics protocol a=a animation control
/// command.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationControl {
    /// Whether the animation is playing
    pub running: bool,
    /// Playing in loading mode: hold on the last frame and wait for
    /// more frames to be transmitted instead of looping
    pub loading: bool,
    /// v= loop count from the protocol: Some(1) loops forever,
    /// Some(n) with n > 1 loops n-1 times and then stops
    pub loop_count: Option<u32>,
    /// Number of loops completed since the loop count was set
    pub loops_done: u32,
    /// 1-based frame number selected by the client via c=;
    /// the renderer seeks to it once per change
    pub paused_frame: Option<u32>,
}

impl Default for AnimationControl {
    fn default() -> Self {
        Self {
            running: true,
            loading: false,
            loop_count: None,
            loops_done: 0,
            paused_frame: None,
        }
    }
}

impl std::fmt::Debug for ImageDataType {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::EncodedFile(data) => fmt
                .debug_struct("EncodedFile")
                .field("data_of_len", &data.len())
                .finish(),
            Self::EncodedLease(lease) => lease.fmt(fmt),
            Self::Rgba8 {
                data,
                width,
                height,
                hash,
            } => fmt
                .debug_struct("Rgba8")
                .field("data_of_len", &data.len())
                .field("width", &width)
                .field("height", &height)
                .field("hash", &hash)
                .finish(),
            Self::AnimRgba8 {
                frames,
                width,
                height,
                durations,
                hashes,
                ..
            } => fmt
                .debug_struct("AnimRgba8")
                .field("frames_of_len", &frames.len())
                .field("width", &width)
                .field("height", &height)
                .field("durations", durations)
                .field("hashes", hashes)
                .finish(),
            Self::DiskBacked {
                hash,
                width,
                height,
                frame_count,
            } => fmt
                .debug_struct("DiskBacked")
                .field("hash", &format_args!("{}", HexSlice(hash)))
                .field("width", width)
                .field("height", height)
                .field("frame_count", frame_count)
                .finish(),
        }
    }
}

impl ImageDataType {
    pub fn new_single_frame(width: u32, height: u32, data: Vec<u8>) -> Self {
        let hash = Self::hash_bytes(&data);
        assert_eq!(
            width * height * 4,
            data.len() as u32,
            "invalid dimensions {}x{} for pixel data of length {}",
            width,
            height,
            data.len()
        );
        Self::Rgba8 {
            width,
            height,
            data,
            hash,
        }
    }

    /// Black pixels
    pub fn placeholder() -> Self {
        let mut data = vec![];
        let size = 8;
        for _ in 0..size * size {
            data.extend_from_slice(&[0, 0, 0, 0xff]);
        }
        ImageDataType::new_single_frame(size, size, data)
    }

    pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    pub fn compute_hash(&self) -> [u8; 32] {
        use sha2::Digest;
        match self {
            ImageDataType::EncodedLease(lease) => lease.content_id().as_hash_bytes(),
            ImageDataType::Rgba8 { hash, .. } => *hash,
            ImageDataType::EncodedFile(data) => ImageDataType::hash_bytes(data),
            ImageDataType::DiskBacked { hash, .. } => *hash,
            ImageDataType::AnimRgba8 {
                frames, durations, ..
            } => {
                let mut hasher = sha2::Sha256::new();
                for data in frames {
                    hasher.update(data);
                }
                for d in durations {
                    let d = d.as_secs_f32();
                    let b = d.to_ne_bytes();
                    hasher.update(b);
                }
                hasher.finalize().into()
            }
        }
    }

    /// The DiskBacked marker that can stand in for this payload once
    /// its bytes are on the disk cache.  None for payloads that are
    /// already on disk in another form (encoded leases manage their
    /// own storage, and DiskBacked needs no marker).
    fn disk_backed_marker(&self) -> Option<Self> {
        match self {
            Self::Rgba8 {
                width,
                height,
                hash,
                ..
            } => Some(Self::DiskBacked {
                hash: *hash,
                width: *width,
                height: *height,
                frame_count: 1,
            }),
            Self::AnimRgba8 {
                width,
                height,
                frames,
                ..
            } => Some(Self::DiskBacked {
                hash: self.compute_hash(),
                width: *width,
                height: *height,
                frame_count: frames.len() as u32,
            }),
            _ => None,
        }
    }

    /// Divides the animation frame durations by the provided
    /// speed_factor, so a factor of 2 will halve the duration.
    /// # Panics
    /// if the speed_factor is negative, non-finite or the result
    /// overflows the allow Duration range.
    pub fn adjust_speed(&mut self, speed_factor: f32) {
        match self {
            Self::AnimRgba8 { durations, .. } => {
                for d in durations {
                    *d = d.mul_f32(1. / speed_factor);
                }
            }
            _ => {}
        }
    }

    #[cfg(feature = "use_image")]
    pub fn dimensions(&self) -> Result<(u32, u32), ImageCellError> {
        fn dimensions_for_data(data: &[u8]) -> image::ImageResult<(u32, u32)> {
            let reader =
                image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
            let (width, height) = reader.into_dimensions()?;

            Ok((width, height))
        }

        match self {
            ImageDataType::EncodedFile(data) => Ok(dimensions_for_data(data)?),
            ImageDataType::EncodedLease(lease) => Ok(dimensions_for_data(&lease.get_data()?)?),
            ImageDataType::AnimRgba8 { width, height, .. }
            | ImageDataType::Rgba8 { width, height, .. }
            | ImageDataType::DiskBacked { width, height, .. } => Ok((*width, *height)),
        }
    }

    /// Migrate an in-memory encoded image blob to on-disk to reduce
    /// the memory footprint
    pub fn swap_out(self) -> Result<Self, ImageCellError> {
        match self {
            Self::EncodedFile(data) => match BlobManager::store(&data) {
                Ok(lease) => Ok(Self::EncodedLease(lease)),
                Err(wezterm_blob_leases::Error::StorageNotInit) => Ok(Self::EncodedFile(data)),
                Err(err) => Err(err.into()),
            },
            other => Ok(other),
        }
    }

    /// Serialize the payload into the container format used by the
    /// image disk cache.  Returns None when the payload cannot be
    /// represented (or cannot be read back from its lease).
    #[cfg(feature = "std")]
    pub fn to_disk_bytes(&self) -> Option<Vec<u8>> {
        disk_format::encode(self)
    }

    /// Reconstruct a payload serialized by [`Self::to_disk_bytes`].
    /// The container is strictly validated; any mismatch returns None.
    /// Decoding never panics on malformed input.
    #[cfg(feature = "std")]
    pub fn from_disk_bytes(buf: &[u8]) -> Option<Self> {
        disk_format::decode(buf)
    }

    /// Reconstruct a payload previously spilled to the image disk
    /// cache under `hash`; the reconstruction must hash identically
    /// to the key.  Returns None when nothing valid is cached.
    #[cfg(feature = "std")]
    pub fn load_from_disk(hash: &[u8; 32]) -> Option<Self> {
        let bytes = wezterm_blob_leases::image_disk_cache::load(hash)?;
        let data = Self::from_disk_bytes(&bytes)?;
        if &data.compute_hash() != hash {
            return None;
        }
        Some(data)
    }

    /// Decode an encoded file into either an Rgba8 or AnimRgba8 variant
    /// if we recognize the file format, otherwise the EncodedFile data
    /// is preserved as is.
    #[cfg(feature = "use_image")]
    pub fn decode(self) -> Self {
        use image::{AnimationDecoder, ImageFormat};

        match self {
            Self::EncodedFile(data) => {
                let format = match image::guess_format(&data) {
                    Ok(format) => format,
                    Err(err) => {
                        log::warn!("Unable to decode raw image data: {:#}", err);
                        return Self::EncodedFile(data);
                    }
                };
                let cursor = std::io::Cursor::new(&*data);
                match format {
                    ImageFormat::Gif => image::codecs::gif::GifDecoder::new(cursor)
                        .and_then(|decoder| decoder.into_frames().collect_frames())
                        .and_then(|frames| {
                            if frames.is_empty() {
                                log::error!("decoded image has 0 frames, using placeholder");
                                Ok(Self::placeholder())
                            } else {
                                Ok(Self::decode_frames(frames))
                            }
                        })
                        .unwrap_or_else(|err| {
                            log::error!(
                                "Unable to parse animated gif: {:#}, trying as single frame",
                                err
                            );
                            Self::decode_single(data)
                        }),
                    ImageFormat::Png => {
                        let decoder = match image::codecs::png::PngDecoder::new(cursor) {
                            Ok(d) => d,
                            _ => return Self::EncodedFile(data),
                        };
                        if decoder.is_apng().unwrap_or(false) {
                            match decoder
                                .apng()
                                .and_then(|d| d.into_frames().collect_frames())
                            {
                                Ok(frames) if frames.is_empty() => {
                                    log::error!("decoded image has 0 frames, using placeholder");
                                    Self::placeholder()
                                }
                                Ok(frames) => Self::decode_frames(frames),
                                _ => Self::EncodedFile(data),
                            }
                        } else {
                            Self::decode_single(data)
                        }
                    }
                    ImageFormat::WebP => {
                        let decoder = match image::codecs::webp::WebPDecoder::new(cursor) {
                            Ok(d) => d,
                            _ => return Self::EncodedFile(data),
                        };
                        match decoder.into_frames().collect_frames() {
                            Ok(frames) if frames.is_empty() => {
                                log::error!("decoded image has 0 frames, using placeholder");
                                Self::placeholder()
                            }
                            Ok(frames) => Self::decode_frames(frames),
                            _ => Self::EncodedFile(data),
                        }
                    }
                    _ => Self::decode_single(data),
                }
            }
            data => data,
        }
    }

    #[cfg(not(feature = "use_image"))]
    pub fn decode(self) -> Self {
        self
    }

    #[cfg(feature = "use_image")]
    fn decode_frames(img_frames: Vec<image::Frame>) -> Self {
        let mut width = 0;
        let mut height = 0;
        let mut frames = vec![];
        let mut durations = vec![];
        let mut hashes = vec![];
        for frame in img_frames.into_iter() {
            let duration: Duration = frame.delay().into();
            durations.push(duration);
            let image = image::DynamicImage::ImageRgba8(frame.into_buffer()).to_rgba8();
            let (w, h) = image.dimensions();
            width = w;
            height = h;
            let data = image.into_vec();
            hashes.push(Self::hash_bytes(&data));
            frames.push(data);
        }
        Self::AnimRgba8 {
            width,
            height,
            frames,
            durations,
            hashes,
            control: AnimationControl::default(),
        }
    }

    #[cfg(feature = "use_image")]
    fn decode_single(data: Vec<u8>) -> Self {
        match image::load_from_memory(&data) {
            Ok(image) => {
                let image = image.to_rgba8();
                let (width, height) = image.dimensions();
                let data = image.into_vec();
                let hash = Self::hash_bytes(&data);
                Self::Rgba8 {
                    width,
                    height,
                    data,
                    hash,
                }
            }
            _ => Self::EncodedFile(data),
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ImageCellError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    BlobLease(#[from] wezterm_blob_leases::Error),

    #[error(transparent)]
    ImageError(#[from] image::ImageError),
}

/// A shareable image: an [`ImageDataType`] behind a `Mutex`,
/// with an immutable content-identity hash captured at creation-time only.
///
/// The hash is the dedup key for this image: `PartialEq` compares hashes only.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
pub struct ImageData {
    data: Mutex<ImageDataType>,
    hash: [u8; 32],
    /// Monotonic stamp of the most recent disk read-back of this
    /// payload, used to order swap-out candidates so that a freshly
    /// displayed image is not immediately swapped out again.
    /// Runtime state only; never serialized.
    #[cfg_attr(feature = "use_serde", serde(skip))]
    restored_tick: AtomicU64,
}

/// Monotonic source of read-back stamps for swapped-out images.
#[cfg(feature = "std")]
static RESTORE_TICK: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "std")]
fn next_restore_tick() -> u64 {
    RESTORE_TICK.fetch_add(1, Ordering::Relaxed) + 1
}

struct HexSlice<'a>(&'a [u8]);
impl<'a> std::fmt::Display for HexSlice<'a> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        for byte in self.0 {
            write!(fmt, "{byte:x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("ImageData")
            .field("data", &self.data)
            .field("hash", &format_args!("{}", HexSlice(&self.hash)))
            .finish()
    }
}

impl Eq for ImageData {}
impl PartialEq for ImageData {
    fn eq(&self, rhs: &Self) -> bool {
        self.hash == rhs.hash
    }
}

impl ImageData {
    /// Create a new ImageData struct with the provided raw data.
    pub fn with_raw_data(data: Vec<u8>) -> Self {
        let hash = ImageDataType::hash_bytes(&data);
        Self::with_data_and_hash(ImageDataType::EncodedFile(data).decode(), hash)
    }

    fn with_data_and_hash(data: ImageDataType, hash: [u8; 32]) -> Self {
        Self {
            data: Mutex::new(data),
            hash,
            restored_tick: AtomicU64::new(0),
        }
    }

    pub fn with_data(data: ImageDataType) -> Self {
        let hash = data.compute_hash();
        Self {
            data: Mutex::new(data),
            hash,
            restored_tick: AtomicU64::new(0),
        }
    }

    /// Returns the in-memory footprint.  Peeks at the payload without
    /// triggering a disk read-back, so that accounting never drags
    /// pixels into RAM; a disk-backed payload counts as zero.
    pub fn len(&self) -> usize {
        match &*self.peek_data() {
            ImageDataType::EncodedFile(d) => d.len(),
            ImageDataType::EncodedLease(_) => 0,
            ImageDataType::Rgba8 { data, .. } => data.len(),
            ImageDataType::AnimRgba8 { frames, .. } => frames.len() * frames[0].len(),
            ImageDataType::DiskBacked { .. } => 0,
        }
    }

    /// Access the payload, reading it back from the image disk cache
    /// first when it was swapped out.  Every holder of this image
    /// shares the same payload, so one read-back restores it for all
    /// of them.  A payload whose disk copy is gone degrades to a
    /// placeholder rather than retrying the read on every access.
    pub fn data(&self) -> MutexGuard<'_, ImageDataType> {
        #[cfg(feature = "std")]
        self.restore_if_swapped();
        self.data.lock().unwrap()
    }

    /// Lock the payload without any read-back; for internal callers
    /// (accounting, swap-out) that must not touch the disk.
    fn peek_data(&self) -> MutexGuard<'_, ImageDataType> {
        self.data.lock().unwrap()
    }

    #[cfg(feature = "std")]
    fn restore_if_swapped(&self) {
        let hash = match &*self.peek_data() {
            ImageDataType::DiskBacked { hash, .. } => *hash,
            _ => return,
        };
        // Reading the payload back is the point where the image is
        // being displayed again; stamp it so that swap-out candidates
        // treat it as most recently used.
        self.restored_tick
            .store(next_restore_tick(), Ordering::Relaxed);
        let restored = match ImageDataType::load_from_disk(&hash) {
            Some(data) => {
                // Account the read-back as a restore, mirroring the
                // spill counter; the by-id restore path builds its
                // image directly and is counted by its caller
                wezterm_blob_leases::image_disk_cache::note_restore_hit();
                data
            }
            None => {
                wezterm_blob_leases::image_disk_cache::note_restore_miss();
                log::error!(
                    "image {} was swapped to the image disk cache but is no \
                     longer recoverable; degrading to a placeholder",
                    HexSlice(&hash)
                );
                ImageDataType::placeholder()
            }
        };
        let mut guard = self.data.lock().unwrap();
        if matches!(&*guard, ImageDataType::DiskBacked { .. }) {
            *guard = restored;
        }
        // Otherwise another thread restored it while we were loading;
        // keep their equally fresh copy
    }

    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    /// Ordering stamp of the most recent disk read-back of this
    /// payload (0 = never read back).
    pub fn restored_tick(&self) -> u64 {
        self.restored_tick.load(Ordering::Relaxed)
    }

    /// Whether the payload is currently swapped out to the image disk
    /// cache.  Peeks; never triggers a read-back.
    #[cfg(feature = "std")]
    pub fn is_disk_backed(&self) -> bool {
        matches!(&*self.peek_data(), ImageDataType::DiskBacked { .. })
    }

    /// Write the payload to the image disk cache under this image's
    /// content hash, so that it can be recovered with
    /// [`Self::load_from_disk`] after the in-memory copy is released.
    /// Returns true when the payload is now on disk; an already
    /// swapped payload stays recoverable under the same key.
    #[cfg(feature = "std")]
    pub fn spill_to_disk(&self) -> bool {
        let guard = self.peek_data();
        if matches!(&*guard, ImageDataType::DiskBacked { .. }) {
            return true;
        }
        let Some(bytes) = guard.to_disk_bytes() else {
            return false;
        };
        wezterm_blob_leases::image_disk_cache::store(self.hash, &bytes)
    }

    /// Reconstruct an image previously written by [`Self::spill_to_disk`].
    /// Returns None when nothing is cached under `hash` or the cached
    /// payload fails validation.
    #[cfg(feature = "std")]
    pub fn load_from_disk(hash: &[u8; 32]) -> Option<Arc<Self>> {
        Some(Arc::new(Self::with_data(ImageDataType::load_from_disk(
            hash,
        )?)))
    }

    /// Swap the pixel payload out to the image disk cache, leaving a
    /// lightweight [`ImageDataType::DiskBacked`] marker in its place.
    /// References held by cells and placements stay valid: a later
    /// [`Self::data`] call reads the pixels back.  Returns true when
    /// the payload is now disk-backed.  Payloads already on disk (in
    /// either form) are left alone, and this never triggers a
    /// read-back of a swapped payload.
    #[cfg(feature = "std")]
    pub fn swap_out_to_disk(&self) -> bool {
        let mut guard = self.peek_data();
        let marker = match guard.disk_backed_marker() {
            Some(marker) => marker,
            None => return matches!(&*guard, ImageDataType::DiskBacked { .. }),
        };
        let hash = guard.compute_hash();
        let Some(bytes) = guard.to_disk_bytes() else {
            return false;
        };
        if !wezterm_blob_leases::image_disk_cache::store(hash, &bytes) {
            return false;
        }
        *guard = marker;
        true
    }
}

/// The container format used by the image disk cache.
///
/// The layout is a fixed little-endian header followed by per-frame
/// entries, chosen so that encoding/decoding is cheap and versioned:
///
/// ```text
/// magic    b"WTID"
/// version  u32        (currently 1)
/// kind     u8         1=Rgba8, 2=AnimRgba8, 3=Encoded
/// width    u32        (0 for Encoded)
/// height   u32        (0 for Encoded)
/// frames   u32
/// control  (AnimRgba8 only) flags:u8, has_loop:u8, [loop:u32],
///          loops_done:u32, has_paused:u8, [paused:u32]
/// frame*   secs:u64 nanos:u32 len:u32 bytes[len]
/// ```
///
/// Reconstruction recomputes the content hashes, so a decoded payload
/// hashes identically to the spilled original; the cache key (the
/// [`ImageData`] hash) therefore survives the round trip.
#[cfg(feature = "std")]
mod disk_format {
    use super::{AnimationControl, ImageDataType};
    use std::time::Duration;

    const MAGIC: &[u8; 4] = b"WTID";
    const VERSION: u32 = 1;
    const KIND_RGBA8: u8 = 1;
    const KIND_ANIM: u8 = 2;
    const KIND_ENCODED: u8 = 3;

    /// A frame entry costs at least its duration, length and zero
    /// payload bytes; used to sanity check frame counts before
    /// allocating.
    const MIN_FRAME_ENTRY: usize = 16;

    struct Writer {
        buf: Vec<u8>,
    }

    impl Writer {
        fn new() -> Self {
            let mut buf = Vec::new();
            buf.extend_from_slice(MAGIC);
            Self { buf }
        }
        fn put_u8(&mut self, v: u8) {
            self.buf.push(v);
        }
        fn put_u32(&mut self, v: u32) {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        fn put_u64(&mut self, v: u64) {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        fn put_bytes(&mut self, v: &[u8]) {
            self.put_u32(v.len() as u32);
            self.buf.extend_from_slice(v);
        }
        fn put_duration(&mut self, d: &Duration) {
            self.put_u64(d.as_secs());
            self.put_u32(d.subsec_nanos());
        }
        fn put_control(&mut self, control: &AnimationControl) {
            let mut flags = 0u8;
            if control.running {
                flags |= 1;
            }
            if control.loading {
                flags |= 2;
            }
            self.put_u8(flags);
            match control.loop_count {
                Some(loop_count) => {
                    self.put_u8(1);
                    self.put_u32(loop_count);
                }
                None => self.put_u8(0),
            }
            self.put_u32(control.loops_done);
            match control.paused_frame {
                Some(paused) => {
                    self.put_u8(1);
                    self.put_u32(paused);
                }
                None => self.put_u8(0),
            }
        }
        fn finish(self) -> Vec<u8> {
            self.buf
        }
    }

    struct Reader<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        fn new(buf: &'a [u8]) -> Self {
            Self { buf, pos: 0 }
        }
        fn take(&mut self, len: usize) -> Option<&'a [u8]> {
            let end = self.pos.checked_add(len)?;
            let slice = self.buf.get(self.pos..end)?;
            self.pos = end;
            Some(slice)
        }
        fn take_u8(&mut self) -> Option<u8> {
            self.take(1).map(|s| s[0])
        }
        fn take_u32(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn take_u64(&mut self) -> Option<u64> {
            Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
        }
        fn take_bytes(&mut self) -> Option<Vec<u8>> {
            let len = self.take_u32()? as usize;
            self.take(len).map(|s| s.to_vec())
        }
        fn take_duration(&mut self) -> Option<Duration> {
            let secs = self.take_u64()?;
            let nanos = self.take_u32()?;
            if nanos > 999_999_999 {
                return None;
            }
            Some(Duration::new(secs, nanos))
        }
        fn take_control(&mut self) -> Option<AnimationControl> {
            let flags = self.take_u8()?;
            let loop_count = match self.take_u8()? {
                1 => Some(self.take_u32()?),
                0 => None,
                _ => return None,
            };
            let loops_done = self.take_u32()?;
            let paused_frame = match self.take_u8()? {
                1 => Some(self.take_u32()?),
                0 => None,
                _ => return None,
            };
            Some(AnimationControl {
                running: flags & 1 != 0,
                loading: flags & 2 != 0,
                loop_count,
                loops_done,
                paused_frame,
            })
        }
    }

    fn expected_frame_len(width: u32, height: u32) -> Option<usize> {
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)
    }

    pub fn encode(data: &ImageDataType) -> Option<Vec<u8>> {
        let mut w = Writer::new();
        w.put_u32(VERSION);
        match data {
            ImageDataType::Rgba8 {
                data,
                width,
                height,
                ..
            } => {
                if expected_frame_len(*width, *height) != Some(data.len()) {
                    return None;
                }
                w.put_u8(KIND_RGBA8);
                w.put_u32(*width);
                w.put_u32(*height);
                w.put_u32(1);
                w.put_duration(&Duration::ZERO);
                w.put_bytes(data);
            }
            ImageDataType::AnimRgba8 {
                width,
                height,
                durations,
                frames,
                control,
                ..
            } => {
                if durations.len() != frames.len() || frames.is_empty() {
                    return None;
                }
                for frame in frames {
                    if expected_frame_len(*width, *height) != Some(frame.len()) {
                        return None;
                    }
                }
                w.put_u8(KIND_ANIM);
                w.put_u32(*width);
                w.put_u32(*height);
                w.put_u32(frames.len() as u32);
                w.put_control(control);
                for (frame, duration) in frames.iter().zip(durations.iter()) {
                    w.put_duration(duration);
                    w.put_bytes(frame);
                }
            }
            ImageDataType::EncodedFile(data) => {
                w.put_u8(KIND_ENCODED);
                w.put_u32(0);
                w.put_u32(0);
                w.put_u32(1);
                w.put_duration(&Duration::ZERO);
                w.put_bytes(data);
            }
            ImageDataType::EncodedLease(lease) => {
                let data = lease.get_data().ok()?;
                w.put_u8(KIND_ENCODED);
                w.put_u32(0);
                w.put_u32(0);
                w.put_u32(1);
                w.put_duration(&Duration::ZERO);
                w.put_bytes(&data);
            }
            // Already on the disk cache; a marker needs no payload
            ImageDataType::DiskBacked { .. } => {
                return None;
            }
        }
        Some(w.finish())
    }

    pub fn decode(buf: &[u8]) -> Option<ImageDataType> {
        let mut r = Reader::new(buf);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.take_u32()? != VERSION {
            return None;
        }
        let kind = r.take_u8()?;
        let width = r.take_u32()?;
        let height = r.take_u32()?;
        let frame_count = r.take_u32()? as usize;
        if frame_count == 0 || frame_count > buf.len() / MIN_FRAME_ENTRY {
            return None;
        }

        let data = match kind {
            KIND_RGBA8 => {
                if frame_count != 1 {
                    return None;
                }
                let frame_len = expected_frame_len(width, height)?;
                let _duration = r.take_duration()?;
                let frame = r.take_bytes()?;
                if frame.len() != frame_len {
                    return None;
                }
                let hash = ImageDataType::hash_bytes(&frame);
                ImageDataType::Rgba8 {
                    width,
                    height,
                    data: frame,
                    hash,
                }
            }
            KIND_ANIM => {
                let control = r.take_control()?;
                let frame_len = expected_frame_len(width, height)?;
                let mut durations = Vec::with_capacity(frame_count);
                let mut frames = Vec::with_capacity(frame_count);
                let mut hashes = Vec::with_capacity(frame_count);
                for _ in 0..frame_count {
                    durations.push(r.take_duration()?);
                    let frame = r.take_bytes()?;
                    if frame.len() != frame_len {
                        return None;
                    }
                    hashes.push(ImageDataType::hash_bytes(&frame));
                    frames.push(frame);
                }
                ImageDataType::AnimRgba8 {
                    width,
                    height,
                    durations,
                    frames,
                    hashes,
                    control,
                }
            }
            KIND_ENCODED => {
                if frame_count != 1 || (width, height) != (0, 0) {
                    return None;
                }
                let _duration = r.take_duration()?;
                ImageDataType::EncodedFile(r.take_bytes()?)
            }
            _ => return None,
        };
        if r.pos != buf.len() {
            // Trailing garbage means corruption, not a truncated write
            return None;
        }
        Some(data)
    }
}

#[cfg(all(test, feature = "std"))]
mod disk_cache_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The image disk cache is a process-global singleton; hold this
    /// lock (shared with the other cache test modules below) so that
    /// tests don't fight over it.
    pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct Serial {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for Serial {
        fn drop(&mut self) {
            // The cache is a process-global static whose Drop never
            // runs at exit; clear it per test or the temp dir leaks
            wezterm_blob_leases::image_disk_cache::clear();
        }
    }

    fn setup_cache() -> Serial {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        wezterm_blob_leases::image_disk_cache::clear();
        wezterm_blob_leases::image_disk_cache::configure(true, usize::MAX, None);
        Serial { _guard }
    }

    fn sample_rgba() -> ImageDataType {
        let (width, height) = (3u32, 2u32);
        let data: Vec<u8> = (0..width * height * 4).map(|i| i as u8).collect();
        ImageDataType::new_single_frame(width, height, data)
    }

    fn sample_anim() -> ImageDataType {
        let (width, height) = (2u32, 2u32);
        let frame_len = (width * height * 4) as usize;
        let frames = vec![vec![1u8; frame_len], vec![2u8; frame_len]];
        let hashes: Vec<[u8; 32]> = frames
            .iter()
            .map(|f| ImageDataType::hash_bytes(f))
            .collect();
        ImageDataType::AnimRgba8 {
            width,
            height,
            durations: vec![Duration::new(1, 500_000_000), Duration::from_millis(250)],
            frames,
            hashes,
            control: AnimationControl {
                running: false,
                loading: true,
                loop_count: Some(3),
                loops_done: 1,
                paused_frame: Some(2),
            },
        }
    }

    /// Every variant survives a to_disk_bytes/from_disk_bytes round
    /// trip, and the reconstruction hashes identically to the original
    #[test]
    fn roundtrip_each_variant() {
        for original in [
            sample_rgba(),
            sample_anim(),
            ImageDataType::EncodedFile(b"png-ish".to_vec()),
        ] {
            let expected_hash = original.compute_hash();
            let restored = ImageDataType::from_disk_bytes(&original.to_disk_bytes().unwrap())
                .expect("decodes");
            assert_eq!(restored, original);
            assert_eq!(restored.compute_hash(), expected_hash);
        }
    }

    /// An image spilled under its hash loads back with the same
    /// payload and hash; unknown hashes load nothing
    #[test]
    fn spill_and_load_via_disk_cache() {
        let _serial = setup_cache();
        let image = ImageData::with_data(sample_anim());
        assert!(image.spill_to_disk());

        let restored = ImageData::load_from_disk(&image.hash()).expect("loads back");
        assert_eq!(*restored.data(), sample_anim());
        assert_eq!(restored.hash(), image.hash());

        let absent = [0xabu8; 32];
        assert!(ImageData::load_from_disk(&absent).is_none());
    }

    /// Malformed containers are rejected instead of panicking
    #[test]
    fn malformed_containers_rejected() {
        let encoded = sample_rgba().to_disk_bytes().unwrap();

        assert!(ImageDataType::from_disk_bytes(&[]).is_none(), "empty");
        assert!(ImageDataType::from_disk_bytes(b"XXXX").is_none(), "short");
        let mut bad_magic = encoded.clone();
        bad_magic[0] = b'X';
        assert!(
            ImageDataType::from_disk_bytes(&bad_magic).is_none(),
            "magic"
        );
        let mut bad_version = encoded.clone();
        bad_version[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(
            ImageDataType::from_disk_bytes(&bad_version).is_none(),
            "version"
        );
        assert!(
            ImageDataType::from_disk_bytes(&encoded[..encoded.len() - 1]).is_none(),
            "truncated"
        );
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(
            ImageDataType::from_disk_bytes(&trailing).is_none(),
            "trailing garbage"
        );

        // A frame length that disagrees with the declared dimensions
        let mut bad_dims = encoded.clone();
        // width sits at offset 9 in the header
        bad_dims[9..13].copy_from_slice(&99u32.to_le_bytes());
        assert!(
            ImageDataType::from_disk_bytes(&bad_dims).is_none(),
            "dimension mismatch"
        );
    }
}

#[cfg(all(test, feature = "std"))]
mod swap_out_tests {
    use super::disk_cache_tests::TEST_LOCK;
    use super::*;
    use std::sync::MutexGuard;

    struct Serial {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for Serial {
        fn drop(&mut self) {
            wezterm_blob_leases::image_disk_cache::clear();
        }
    }

    fn setup_cache() -> Serial {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        wezterm_blob_leases::image_disk_cache::clear();
        wezterm_blob_leases::image_disk_cache::configure(true, usize::MAX, None);
        Serial { _guard }
    }

    fn sample_rgba() -> ImageDataType {
        let (width, height) = (3u32, 2u32);
        let data: Vec<u8> = (0..width * height * 4).map(|i| i as u8).collect();
        ImageDataType::new_single_frame(width, height, data)
    }

    fn sample_anim() -> ImageDataType {
        let (width, height) = (2u32, 2u32);
        let frame_len = (width * height * 4) as usize;
        let frames = vec![vec![1u8; frame_len], vec![2u8; frame_len]];
        let hashes: Vec<[u8; 32]> = frames
            .iter()
            .map(|f| ImageDataType::hash_bytes(f))
            .collect();
        ImageDataType::AnimRgba8 {
            width,
            height,
            durations: vec![Duration::from_millis(500), Duration::from_millis(250)],
            frames,
            hashes,
            control: AnimationControl {
                running: false,
                loading: true,
                loop_count: Some(3),
                loops_done: 1,
                paused_frame: Some(2),
            },
        }
    }

    /// swap_out_to_disk parks a static pixel payload on the disk cache;
    /// accounting sees no footprint while data() reads the pixels back
    /// in place and stamps the read-back.
    #[test]
    fn swap_out_and_read_back_rgba() {
        let _serial = setup_cache();
        let image = ImageData::with_data(sample_rgba());
        let payload_len = image.len();
        assert!(image.swap_out_to_disk());

        assert!(image.is_disk_backed());
        assert_eq!(image.len(), 0, "disk-backed payload has no footprint");
        assert_eq!(image.restored_tick(), 0, "not read back yet");
        // Dimensions come from the marker without dragging pixels back
        assert_eq!(image.peek_data().dimensions().unwrap(), (3, 2));
        assert_eq!(image.len(), 0);

        // Accessing the payload reads it back transparently
        drop(image.data());
        assert!(!image.is_disk_backed());
        assert_eq!(image.len(), payload_len);
        assert!(image.restored_tick() > 0, "read-back stamped");
        assert_eq!(*image.data(), sample_rgba());
    }

    /// Animated payloads survive a swap/read-back round trip with
    /// durations and playback state intact.
    #[test]
    fn swap_out_and_read_back_anim() {
        let _serial = setup_cache();
        let image = ImageData::with_data(sample_anim());
        assert!(image.swap_out_to_disk());
        assert!(image.is_disk_backed());

        drop(image.data());

        assert!(!image.is_disk_backed());
        assert_eq!(*image.data(), sample_anim());
    }

    /// Encoded payloads keep their own on-disk management (blob
    /// leases) and are not swapped by swap_out_to_disk.
    #[test]
    fn swap_out_leaves_encoded_payloads_alone() {
        let _serial = setup_cache();
        let image = ImageData::with_data(ImageDataType::EncodedFile(b"png-ish".to_vec()));
        assert!(!image.swap_out_to_disk());
        assert!(!image.is_disk_backed());
    }

    /// A swapped payload whose disk copy vanished (quota eviction)
    /// degrades to a placeholder rather than retrying the read on
    /// every access.
    #[test]
    fn read_back_failure_degrades_to_placeholder() {
        let _serial = setup_cache();
        let image = ImageData::with_data(sample_rgba());
        assert!(image.swap_out_to_disk());
        let hash = image.hash();
        wezterm_blob_leases::image_disk_cache::remove(&hash);

        drop(image.data());

        assert!(!image.is_disk_backed());
        assert_eq!(
            image.len(),
            8 * 8 * 4,
            "placeholder footprint after failed read-back"
        );
    }
}
