use crate::allocate::*;
use crate::osc::{base64_decode, base64_encode};
use core::fmt::{Display, Error as FmtError, Formatter};

fn get<'a>(keys: &BTreeMap<&str, &'a str>, k: &str) -> Option<&'a str> {
    keys.get(k).map(|&s| s)
}

fn geti<T: core::str::FromStr>(keys: &BTreeMap<&str, &str>, k: &str) -> Option<T> {
    get(keys, k).and_then(|s| s.parse().ok())
}

fn set<T: ToString>(keys: &mut BTreeMap<&'static str, String>, k: &'static str, v: &Option<T>) {
    if let Some(v) = v {
        keys.insert(k, v.to_string());
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum KittyImageData {
    /// The data bytes, baes64-encoded fragments.
    /// t='d'
    Direct(String),
    DirectBin(Vec<u8>),
    /// The path to a file containing the data.
    /// t='f'
    File {
        path: String,
        /// the amount of data to read.
        /// S=...
        data_size: Option<u32>,
        /// The offset at which to read.
        /// O=...
        data_offset: Option<u32>,
    },
    /// The path to a temporary file containing the data.
    /// If the path is in a known temporary location,
    /// it should be removed once the data has been read
    /// t='t'
    TemporaryFile {
        path: String,
        /// the amount of data to read.
        /// S=...
        data_size: Option<u32>,
        /// The offset at which to read.
        /// O=...
        data_offset: Option<u32>,
    },

    /// The name of a shared memory object.
    /// Can be opened via shm_open() and then should be removed
    /// via shm_unlink().
    /// On Windows, OpenFileMapping(), MapViewOfFile(), UnmapViewOfFile()
    /// and CloseHandle() are used to access and release the data.
    /// t='s'
    SharedMem {
        name: String,
        /// the amount of data to read.
        /// S=...
        data_size: Option<u32>,
        /// The offset at which to read.
        /// O=...
        data_offset: Option<u32>,
    },
}

impl core::fmt::Debug for KittyImageData {
    fn fmt(&self, fmt: &mut Formatter) -> core::fmt::Result {
        match self {
            Self::Direct(data) => write!(fmt, "Direct({} bytes of data)", data.len()),
            Self::DirectBin(data) => write!(fmt, "DirectBin({} bytes of data)", data.len()),
            Self::File {
                path,
                data_offset,
                data_size,
            } => fmt
                .debug_struct("File")
                .field("path", &path)
                .field("data_offset", &data_offset)
                .field("data_size", data_size)
                .finish(),
            Self::TemporaryFile {
                path,
                data_offset,
                data_size,
            } => fmt
                .debug_struct("TemporaryFile")
                .field("path", &path)
                .field("data_offset", &data_offset)
                .field("data_size", data_size)
                .finish(),
            Self::SharedMem {
                name,
                data_offset,
                data_size,
            } => fmt
                .debug_struct("SharedMem")
                .field("name", &name)
                .field("data_offset", &data_offset)
                .field("data_size", data_size)
                .finish(),
        }
    }
}

impl KittyImageData {
    fn from_keys(keys: &BTreeMap<&str, &str>, payload: &[u8]) -> Option<Self> {
        let t = get(keys, "t").unwrap_or("d");

        match t {
            "d" => Some(Self::Direct(String::from_utf8(payload.to_vec()).ok()?)),
            "f" => Some(Self::File {
                path: String::from_utf8(base64_decode(payload.to_vec()).ok()?).ok()?,
                data_size: match geti(keys, "S") {
                    None | Some(0) => None,
                    n => n,
                },
                data_offset: geti(keys, "O"),
            }),
            "t" => Some(Self::TemporaryFile {
                path: String::from_utf8(base64_decode(payload.to_vec()).ok()?).ok()?,
                data_size: match geti(keys, "S") {
                    None | Some(0) => None,
                    n => n,
                },
                data_offset: geti(keys, "O"),
            }),
            "s" => Some(Self::SharedMem {
                name: String::from_utf8(base64_decode(payload.to_vec()).ok()?).ok()?,
                data_size: match geti(keys, "S") {
                    None | Some(0) => None,
                    n => n,
                },
                data_offset: geti(keys, "O"),
            }),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::Direct(d) => {
                keys.insert("payload", d.to_string());
            }
            Self::DirectBin(d) => {
                keys.insert("payload", base64_encode(d));
            }
            Self::File {
                path,
                data_offset,
                data_size,
            } => {
                keys.insert("t", "f".to_string());
                keys.insert("payload", base64_encode(&path));
                set(keys, "S", data_size);
                set(keys, "O", data_offset);
            }
            Self::TemporaryFile {
                path,
                data_offset,
                data_size,
            } => {
                keys.insert("t", "t".to_string());
                keys.insert("payload", base64_encode(&path));
                set(keys, "S", data_size);
                set(keys, "O", data_offset);
            }
            Self::SharedMem {
                name,
                data_offset,
                data_size,
            } => {
                keys.insert("t", "s".to_string());
                keys.insert("payload", base64_encode(&name));
                set(keys, "S", data_size);
                set(keys, "O", data_offset);
            }
        }
    }

    /// Take the image data bytes.
    /// This operation is not repeatable as some of the sources require
    /// removing the underlying file or shared memory object as part
    /// of the read operaiton.
    #[cfg(feature = "kitty-shm")]
    pub fn load_data(self) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek};
        fn read_from_file(
            path: &str,
            data_offset: Option<u32>,
            data_size: Option<u32>,
        ) -> std::io::Result<Vec<u8>> {
            let mut f = std::fs::File::open(path)?;
            if let Some(offset) = data_offset {
                f.seek(std::io::SeekFrom::Start(offset.into()))?;
            }
            if let Some(len) = data_size {
                let mut res = vec![0u8; len as usize];
                f.read_exact(&mut res)?;
                Ok(res)
            } else {
                let mut res = vec![];
                f.read_to_end(&mut res)?;
                Ok(res)
            }
        }

        match self {
            Self::Direct(data) => base64_decode(data).or_else(|err| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("base64 decode: {err:#}"),
                ))
            }),
            Self::DirectBin(bin) => Ok(bin),
            Self::File {
                path,
                data_offset,
                data_size,
            } => read_from_file(&path, data_offset, data_size),
            Self::TemporaryFile {
                path,
                data_offset,
                data_size,
            } => {
                let data = read_from_file(&path, data_offset, data_size)?;
                // need to sanity check that the path looks like a reasonable
                // temporary directory path before blindly unlinking it here.

                fn looks_like_temp_path(p: &str) -> bool {
                    if p.starts_with("/tmp/")
                        || p.starts_with("/var/tmp/")
                        || p.starts_with("/dev/shm/")
                    {
                        return true;
                    }

                    if let Ok(t) = std::env::var("TMPDIR") {
                        if p.starts_with(&t) {
                            return true;
                        }
                    }

                    false
                }

                if looks_like_temp_path(&path) {
                    if let Err(err) = std::fs::remove_file(&path) {
                        log::error!(
                            "Unable to remove kitty image protocol temporary file {}: {:#}",
                            path,
                            err
                        );
                    }
                } else {
                    log::warn!(
                        "kitty image protocol temporary file {} isn't in a known \
                                temporary directory; won't try to remove it",
                        path
                    );
                }

                Ok(data)
            }
            Self::SharedMem {
                name,
                data_offset,
                data_size,
            } => read_shared_memory_data(&name, data_offset, data_size),
        }
    }
}

/// Read the data a `t=s` transmission left in a POSIX shared memory object.
///
/// A shared memory object is not a stream on every platform: on Darwin it can
/// only be mapped, and answers `read(2)` with `ENXIO` and `lseek(2)` with
/// `ESPIPE`. Mapping it works everywhere, and is what the Windows
/// implementation below does as well.
#[cfg(all(feature = "kitty-shm", unix, not(target_os = "android")))]
fn read_shared_memory_data(
    name: &str,
    data_offset: Option<u32>,
    data_size: Option<u32>,
) -> std::result::Result<std::vec::Vec<u8>, std::io::Error> {
    use nix::sys::mman::{shm_open, shm_unlink};
    use std::fs::File;

    let fd = shm_open(
        name,
        nix::fcntl::OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("shm_open {} failed: {:#}", name, err),
        )
    })?;

    // The protocol asks the terminal to unlink the object once it has read it.
    // Unlinking as soon as it is open keeps the descriptor alive for the read
    // and narrows the window in which an error path leaves the object behind.
    if let Err(err) = shm_unlink(name) {
        log::warn!(
            "Unable to unlink kitty image protocol shm file {}: {:#}",
            name,
            err
        );
    }

    let file = File::from(fd);
    if file.metadata()?.len() == 0 {
        return Ok(std::vec::Vec::new());
    }

    // The mapping covers the whole object and `data_offset` indexes into it,
    // because mmap only accepts a page aligned offset while the protocol places
    // no such limit on `O`.
    //
    // SAFETY: a mapping is unsound if the object is written or truncated while
    // it is mapped, and nothing here can prevent that. The unlink above is no
    // help: it takes away the name, not a descriptor already open on it, and
    // the client that staged the data holds one. What this rests on is `t=s`
    // meaning the client has finished writing and handed the object over. The
    // mapping lives only for the copy below.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("mmap {} failed: {:#}", name, err),
        )
    })?;

    // The size an object reports can exceed what the client staged in it, as
    // Darwin rounds it up to a page. `data_size` is what narrows it back down.
    let offset = data_offset.unwrap_or(0) as usize;
    if offset >= map.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "offset {} bigger than or equal to shm region size {}",
                offset,
                map.len()
            ),
        ));
    }
    let mut size = map.len() - offset;
    if let Some(val) = data_size {
        size = size.min(val as usize);
    }

    Ok(map[offset..offset + size].to_vec())
}

#[cfg(all(feature = "kitty-shm", unix, target_os = "android"))]
fn read_shared_memory_data(
    _name: &str,
    _data_offset: Option<u32>,
    _data_size: Option<u32>,
) -> std::result::Result<std::vec::Vec<u8>, std::io::Error> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(all(feature = "kitty-shm", windows))]
mod win {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::memoryapi::{
        FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery,
    };
    use winapi::um::winnt::{HANDLE, MEMORY_BASIC_INFORMATION};

    struct HandleWrapper {
        handle: HANDLE,
    }

    struct SharedMemObject {
        _handle: HandleWrapper,
        buf: *mut u8,
    }

    impl Drop for HandleWrapper {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    impl Drop for SharedMemObject {
        fn drop(&mut self) {
            unsafe {
                UnmapViewOfFile(self.buf as _);
            }
        }
    }

    /// Convert a rust string to a windows wide string
    fn wide_string(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn read_shared_memory_data(
        name: &str,
        data_offset: Option<u32>,
        data_size: Option<u32>,
    ) -> std::result::Result<std::vec::Vec<u8>, std::io::Error> {
        let wide_name = wide_string(&name);

        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide_name.as_ptr()) };
        if handle.is_null() {
            let err = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("OpenFileMappingW {} failed: {:#}", name, err),
            ));
        }

        let handle_wrapper = HandleWrapper { handle };
        let buf = unsafe { MapViewOfFile(handle_wrapper.handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if buf.is_null() {
            let err = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("MapViewOfFile failed: {:#}", err),
            ));
        }

        let shm = SharedMemObject {
            _handle: handle_wrapper,
            buf: buf as *mut u8,
        };

        let mut memory_info = MEMORY_BASIC_INFORMATION::default();
        let res = unsafe {
            VirtualQuery(
                shm.buf as _,
                &mut memory_info as *mut MEMORY_BASIC_INFORMATION,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if res == 0 {
            let err = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "Can't get the size of Shared Memory, VirtualQuery failed: {:#}",
                    err
                ),
            ));
        }
        let mut size = memory_info.RegionSize;
        let offset = data_offset.unwrap_or(0) as usize;
        if offset >= size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "offset {} bigger than or equal to shm region size {}",
                    offset, size
                ),
            ));
        }
        size = size.saturating_sub(offset);
        if let Some(val) = data_size {
            size = size.min(val as usize);
        }
        let buf_slice = unsafe { std::slice::from_raw_parts(shm.buf.add(offset), size) };
        let data = buf_slice.to_vec();

        Ok(data)
    }
}

#[cfg(all(feature = "kitty-shm", windows))]
use win::read_shared_memory_data;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum KittyImageVerbosity {
    Verbose,
    OnlyErrors,
    Quiet,
}

impl KittyImageVerbosity {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        match get(keys, "q") {
            None | Some("0") => Some(Self::Verbose),
            Some("1") => Some(Self::OnlyErrors),
            Some("2") => Some(Self::Quiet),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::Verbose => {}
            Self::OnlyErrors => {
                keys.insert("q", "1".to_string());
            }
            Self::Quiet => {
                keys.insert("q", "2".to_string());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KittyImageFormat {
    /// f=24
    Rgb,
    /// f=32
    Rgba,
    /// f=100
    Png,
}

impl KittyImageFormat {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Option<Self>> {
        match get(keys, "f") {
            None => Some(None),
            Some("32") => Some(Some(Self::Rgba)),
            Some("24") => Some(Some(Self::Rgb)),
            Some("100") => Some(Some(Self::Png)),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::Rgb => keys.insert("f", "24".to_string()),
            Self::Rgba => keys.insert("f", "32".to_string()),
            Self::Png => keys.insert("f", "100".to_string()),
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KittyImageCompression {
    None,
    /// o='z'
    Deflate,
}

impl KittyImageCompression {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        match get(keys, "o") {
            None => Some(Self::None),
            Some("z") => Some(Self::Deflate),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::None => {}
            Self::Deflate => {
                keys.insert("o", "z".to_string());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImageTransmit {
    /// f=...
    pub format: Option<KittyImageFormat>,
    /// combination of t=... and d=...
    pub data: KittyImageData,
    /// s=...
    pub width: Option<u32>,
    /// v=...
    pub height: Option<u32>,
    /// The image id.
    /// i=...
    pub image_id: Option<u32>,
    /// The image number
    /// I=...
    pub image_number: Option<u32>,
    /// o=...
    pub compression: KittyImageCompression,

    /// m=0 or m=1
    pub more_data_follows: bool,
}

impl KittyImageTransmit {
    fn from_keys(keys: &BTreeMap<&str, &str>, payload: &[u8]) -> Option<Self> {
        Some(Self {
            format: KittyImageFormat::from_keys(keys)?,
            data: KittyImageData::from_keys(keys, payload)?,
            compression: KittyImageCompression::from_keys(keys)?,
            width: geti(keys, "s"),
            height: geti(keys, "v"),
            image_id: geti(keys, "i"),
            image_number: geti(keys, "I"),
            more_data_follows: match get(keys, "m") {
                None | Some("0") => false,
                Some("1") => true,
                _ => return None,
            },
        })
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        if let Some(f) = &self.format {
            f.to_keys(keys);
        }

        set(keys, "s", &self.width);
        set(keys, "v", &self.height);
        set(keys, "i", &self.image_id);
        set(keys, "I", &self.image_number);
        if self.more_data_follows {
            keys.insert("m", "1".to_string());
        }

        self.compression.to_keys(keys);
        self.data.to_keys(keys);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    /// source rectangle bounds.
    /// Default is whole image.
    /// x=...
    pub x: Option<u32>,
    pub y: Option<u32>,
    pub w: Option<u32>,
    pub h: Option<u32>,
    /// Place the image at an offset from the cell.
    /// X,Y must be <= cell metrics
    /// X=...
    pub x_offset: Option<u32>,
    /// Y=...
    pub y_offset: Option<u32>,
    /// Scale so that the image fits within this number of columns
    /// c=...
    pub columns: Option<u32>,
    /// Scale so that the image fits within this number of rows
    /// r=...
    pub rows: Option<u32>,
    /// By default, cursor will move to after the bottom right
    /// cell of the image placement.  do_not_move_cursor cursor
    /// set to true prevents that.
    /// C=0, C=1
    pub do_not_move_cursor: bool,
    /// Give an explicit placement id to this placement.
    /// p=...
    pub placement_id: Option<u32>,
    /// z=...
    pub z_index: Option<i32>,
}

impl KittyImagePlacement {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        Some(Self {
            x: geti(keys, "x"),
            y: geti(keys, "y"),
            // w, h, c and r all default to zero, and zero is not a rectangle of
            // nothing: for w and h the protocol says the entire width and height
            // are used. It gives c and r the same default without defining what
            // a zero means, so they are read the same way.
            w: match geti(keys, "w") {
                None | Some(0) => None,
                n => n,
            },
            h: match geti(keys, "h") {
                None | Some(0) => None,
                n => n,
            },
            x_offset: geti(keys, "X"),
            y_offset: geti(keys, "Y"),
            columns: match geti(keys, "c") {
                None | Some(0) => None,
                n => n,
            },
            rows: match geti(keys, "r") {
                None | Some(0) => None,
                n => n,
            },
            placement_id: geti(keys, "p"),
            do_not_move_cursor: match get(keys, "C") {
                None | Some("0") => false,
                Some("1") => true,
                _ => return None,
            },
            z_index: geti(keys, "z"),
        })
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        set(keys, "x", &self.x);
        set(keys, "y", &self.y);
        set(keys, "w", &self.w);
        set(keys, "h", &self.h);
        set(keys, "X", &self.x_offset);
        set(keys, "Y", &self.y_offset);
        set(keys, "c", &self.columns);
        set(keys, "r", &self.rows);
        set(keys, "p", &self.placement_id);

        if self.do_not_move_cursor {
            keys.insert("C", "1".to_string());
        }

        set(keys, "z", &self.z_index);
    }
}

/// When the uppercase form is used, the delete: field is set to true
/// which means that the underlying data is also released.  Otherwise,
/// the data is available to be placed again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KittyImageDelete {
    /// d='a' or d='A'.
    /// Delete all placements on visible screen
    All { delete: bool },
    /// d='i' or d='I'
    /// Delete all images with specified image_id.
    /// If placement_id is specified, then both image_id
    /// and placement_id must match
    ByImageId {
        image_id: u32,
        placement_id: Option<u32>,
        delete: bool,
    },
    /// d='n' or d='N'
    /// Delete newest image with specified image number.
    /// If placement_id is specified, then placement_id
    /// must also match.
    ByImageNumber {
        image_number: u32,
        placement_id: Option<u32>,
        delete: bool,
    },

    /// d='c' or d='C'
    /// Delete all placements that intersect with the current
    /// cursor position.
    AtCursorPosition { delete: bool },

    /// d='f' or d='F'
    /// Delete animation frames
    AnimationFrames { delete: bool },

    /// d='p' or d='P'
    /// Delete all placements that intersect the specified
    /// cell x and y coordinates
    DeleteAt { x: u32, y: u32, delete: bool },

    /// d='q' or d='Q'
    /// Delete all placements that intersect the specified
    /// cell x and y coordinates, with the specified z-index
    DeleteAtZ {
        x: u32,
        y: u32,
        z: i32,
        delete: bool,
    },

    /// d='x' or d='X'
    /// Delete all placements that intersect the specified column.
    DeleteColumn { x: u32, delete: bool },

    /// d='y' or d='Y'
    /// Delete all placements that intersect the specified row.
    DeleteRow { y: u32, delete: bool },

    /// d='z' or d='Z'
    /// Delete all placements that have the specified z-index.
    DeleteZ { z: i32, delete: bool },
}

impl KittyImageDelete {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        let d = get(keys, "d").unwrap_or("a");
        if d.len() != 1 {
            return None;
        }
        let d = d.chars().next()?;
        let delete = d.is_ascii_uppercase();
        match d {
            'a' | 'A' => Some(Self::All { delete }),
            'i' | 'I' => Some(Self::ByImageId {
                image_id: geti(keys, "i")?,
                placement_id: geti(keys, "p"),
                delete,
            }),
            'n' | 'N' => Some(Self::ByImageNumber {
                image_number: geti(keys, "I")?,
                placement_id: geti(keys, "p"),
                delete,
            }),
            'c' | 'C' => Some(Self::AtCursorPosition { delete }),
            'f' | 'F' => Some(Self::AnimationFrames { delete }),
            'p' | 'P' => Some(Self::DeleteAt {
                x: geti(keys, "x")?,
                y: geti(keys, "y")?,
                delete,
            }),
            'q' | 'Q' => Some(Self::DeleteAtZ {
                x: geti(keys, "x")?,
                y: geti(keys, "y")?,
                z: geti(keys, "z")?,
                delete,
            }),
            'x' | 'X' => Some(Self::DeleteColumn {
                x: geti(keys, "x")?,
                delete,
            }),
            'y' | 'Y' => Some(Self::DeleteRow {
                y: geti(keys, "y")?,
                delete,
            }),
            'z' | 'Z' => Some(Self::DeleteZ {
                z: geti(keys, "z")?,
                delete,
            }),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        fn d(c: char, delete: &bool) -> String {
            if *delete { c.to_ascii_uppercase() } else { c }.to_string()
        }

        match self {
            Self::All { delete } => {
                keys.insert("d", d('a', delete));
            }
            Self::ByImageId {
                image_id,
                placement_id,
                delete,
            } => {
                keys.insert("d", d('i', delete));
                if let Some(p) = placement_id {
                    keys.insert("p", p.to_string());
                }
                keys.insert("i", image_id.to_string());
            }
            Self::ByImageNumber {
                image_number,
                placement_id,
                delete,
            } => {
                keys.insert("d", d('n', delete));
                if let Some(p) = placement_id {
                    keys.insert("p", p.to_string());
                }
                keys.insert("I", image_number.to_string());
            }
            Self::AtCursorPosition { delete } => {
                keys.insert("d", d('c', delete));
            }
            Self::AnimationFrames { delete } => {
                keys.insert("d", d('f', delete));
            }
            Self::DeleteAt { x, y, delete } => {
                keys.insert("d", d('p', delete));
                keys.insert("x", x.to_string());
                keys.insert("y", y.to_string());
            }
            Self::DeleteAtZ { x, y, z, delete } => {
                keys.insert("d", d('q', delete));
                keys.insert("x", x.to_string());
                keys.insert("y", y.to_string());
                keys.insert("z", z.to_string());
            }
            Self::DeleteColumn { x, delete } => {
                keys.insert("d", d('x', delete));
                keys.insert("x", x.to_string());
            }
            Self::DeleteRow { y, delete } => {
                keys.insert("d", d('y', delete));
                keys.insert("y", y.to_string());
            }
            Self::DeleteZ { z, delete } => {
                keys.insert("d", d('z', delete));
                keys.insert("z", z.to_string());
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyFrameCompositionMode {
    AlphaBlending,
    Overwrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImageFrameCompose {
    /// i=...
    pub image_id: Option<u32>,
    /// I=...
    pub image_number: Option<u32>,

    /// 1-based number of the frame which should be the base
    /// data for the new frame being created.
    /// If omitted, use background_pixel to specify color.
    /// c=...
    pub target_frame: Option<u32>,

    /// 1-based number of the frame which should be edited.
    /// If omitted, a new frame is created.
    /// r=...
    pub source_frame: Option<u32>,

    /// Left edge in pixels to update
    /// x=...
    pub x: Option<u32>,
    /// Top edge in pixels to update
    /// y=...
    pub y: Option<u32>,

    /// Width (in pixels) of the source and destination rectangles.
    /// By default the full width is used.
    /// w=...
    pub w: Option<u32>,

    /// Height (in pixels) of the source and destination rectangles.
    /// By default the full height is used.
    /// h=...
    pub h: Option<u32>,

    /// Left edge in pixels of the source rectangle
    /// X=...
    pub src_x: Option<u32>,
    /// Top edge in pixels of the source rectangle
    /// Y=...
    pub src_y: Option<u32>,

    /// Composition mode.
    /// Default is AlphaBlending
    /// C=...
    pub composition_mode: KittyFrameCompositionMode,
}

impl KittyImageFrameCompose {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        Some(Self {
            image_id: geti(keys, "i"),
            image_number: geti(keys, "I"),
            x: geti(keys, "x"),
            y: geti(keys, "y"),
            src_x: geti(keys, "X"),
            src_y: geti(keys, "Y"),
            w: geti(keys, "w"),
            h: geti(keys, "h"),
            target_frame: match geti(keys, "c") {
                None | Some(0) => None,
                n => n,
            },
            source_frame: match geti(keys, "r") {
                None | Some(0) => None,
                n => n,
            },
            composition_mode: match geti(keys, "C") {
                None | Some(0) => KittyFrameCompositionMode::AlphaBlending,
                Some(1) => KittyFrameCompositionMode::Overwrite,
                _ => return None,
            },
        })
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        set(keys, "i", &self.image_id);
        set(keys, "I", &self.image_number);
        set(keys, "w", &self.w);
        set(keys, "h", &self.h);
        set(keys, "x", &self.x);
        set(keys, "y", &self.y);
        set(keys, "X", &self.src_x);
        set(keys, "Y", &self.src_y);
        set(keys, "c", &self.target_frame);
        set(keys, "r", &self.source_frame);
        match &self.composition_mode {
            KittyFrameCompositionMode::AlphaBlending => {}
            KittyFrameCompositionMode::Overwrite => {
                keys.insert("C", "1".to_string());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImageFrame {
    /// Left edge in pixels to update
    pub x: Option<u32>,
    /// Top edge in pixels to update
    pub y: Option<u32>,

    /// 1-based number of the frame which should be the base
    /// data for the new frame being created.
    /// If omitted, use background_pixel to specify color.
    /// c=...
    pub base_frame: Option<u32>,

    /// 1-based number of the frame which should be edited.
    /// If omitted, a new frame is created.
    /// r=...
    pub frame_number: Option<u32>,

    /// Gap in milliseconds of this frame from the next one.
    /// Zero or omitted values are interpreted as 40ms.
    /// A negative gap asks for a gapless frame, which is not supported.
    /// z=...
    pub duration_ms: Option<u32>,

    /// Composition mode.
    /// Default is AlphaBlending
    /// X=...
    pub composition_mode: KittyFrameCompositionMode,

    /// Background color for pixels not specified in the frame data.
    /// If omitted, use a black, fully-transparent pixel (0)
    /// Y=...
    pub background_pixel: Option<u32>,
}

impl KittyImageFrame {
    fn from_keys(keys: &BTreeMap<&str, &str>) -> Option<Self> {
        Some(Self {
            x: geti(keys, "x"),
            y: geti(keys, "y"),
            base_frame: match geti(keys, "c") {
                None | Some(0) => None,
                n => n,
            },
            frame_number: match geti(keys, "r") {
                None | Some(0) => None,
                n => n,
            },
            duration_ms: match geti(keys, "z") {
                None | Some(0) => None,
                n => n,
            },
            composition_mode: match geti(keys, "X") {
                None | Some(0) => KittyFrameCompositionMode::AlphaBlending,
                Some(1) => KittyFrameCompositionMode::Overwrite,
                _ => return None,
            },
            background_pixel: geti(keys, "Y"),
        })
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        set(keys, "x", &self.x);
        set(keys, "y", &self.y);
        set(keys, "c", &self.base_frame);
        set(keys, "r", &self.frame_number);
        set(keys, "z", &self.duration_ms);
        match &self.composition_mode {
            KittyFrameCompositionMode::AlphaBlending => {}
            KittyFrameCompositionMode::Overwrite => {
                keys.insert("X", "1".to_string());
            }
        }
        set(keys, "Y", &self.background_pixel);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KittyImage {
    /// a='t'
    TransmitData {
        transmit: KittyImageTransmit,
        verbosity: KittyImageVerbosity,
    },
    /// a='T'
    TransmitDataAndDisplay {
        transmit: KittyImageTransmit,
        placement: KittyImagePlacement,
        verbosity: KittyImageVerbosity,
    },
    /// a='p'
    Display {
        image_id: Option<u32>,
        image_number: Option<u32>,
        placement: KittyImagePlacement,
        verbosity: KittyImageVerbosity,
    },
    /// a='d'
    Delete {
        what: KittyImageDelete,
        verbosity: KittyImageVerbosity,
    },
    /// a='q'
    Query { transmit: KittyImageTransmit },
    /// a='f'
    TransmitFrame {
        transmit: KittyImageTransmit,
        frame: KittyImageFrame,
        verbosity: KittyImageVerbosity,
    },
    /// a='c'
    ComposeFrame {
        frame: KittyImageFrameCompose,
        verbosity: KittyImageVerbosity,
    },
}

impl KittyImage {
    pub fn verbosity(&self) -> KittyImageVerbosity {
        match self {
            Self::TransmitData { verbosity, .. } => *verbosity,
            Self::Query { .. } => KittyImageVerbosity::Verbose,
            Self::TransmitDataAndDisplay { verbosity, .. } => *verbosity,
            Self::Display { verbosity, .. } => *verbosity,
            Self::Delete { verbosity, .. } => *verbosity,
            Self::TransmitFrame { verbosity, .. } => *verbosity,
            Self::ComposeFrame { verbosity, .. } => *verbosity,
        }
    }

    pub fn parse_apc(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data[0] != b'G' {
            return None;
        }
        let mut keys_payload_iter = data[1..].splitn(2, |&d| d == b';');
        let keys = keys_payload_iter.next()?;
        let key_string = core::str::from_utf8(keys).ok()?;
        let mut keys: BTreeMap<&str, &str> = BTreeMap::new();
        for k_v in key_string.split(',') {
            let mut k_v = k_v.splitn(2, '=');
            let k = k_v.next()?;
            let v = k_v.next()?;
            keys.insert(k, v);
        }

        let payload = keys_payload_iter.next().unwrap_or(b"");
        let action = get(&keys, "a").unwrap_or("t");
        let verbosity = KittyImageVerbosity::from_keys(&keys)?;
        match action {
            "t" => Some(Self::TransmitData {
                transmit: KittyImageTransmit::from_keys(&keys, payload)?,
                verbosity,
            }),
            "q" => Some(Self::Query {
                transmit: KittyImageTransmit::from_keys(&keys, payload)?,
            }),
            "T" => Some(Self::TransmitDataAndDisplay {
                transmit: KittyImageTransmit::from_keys(&keys, payload)?,
                placement: KittyImagePlacement::from_keys(&keys)?,
                verbosity,
            }),
            "p" => Some(Self::Display {
                placement: KittyImagePlacement::from_keys(&keys)?,
                image_id: geti(&keys, "i"),
                image_number: geti(&keys, "I"),
                verbosity,
            }),
            "d" => Some(Self::Delete {
                what: KittyImageDelete::from_keys(&keys)?,
                verbosity,
            }),
            "f" => Some(Self::TransmitFrame {
                transmit: KittyImageTransmit::from_keys(&keys, payload)?,
                frame: KittyImageFrame::from_keys(&keys)?,
                verbosity,
            }),
            "c" => Some(Self::ComposeFrame {
                frame: KittyImageFrameCompose::from_keys(&keys)?,
                verbosity,
            }),
            _ => None,
        }
    }

    fn to_keys(&self, keys: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::TransmitData {
                transmit,
                verbosity,
            } => {
                // Implied: keys.insert("a", "t".to_string());
                verbosity.to_keys(keys);
                transmit.to_keys(keys);
            }
            Self::Query { transmit } => {
                keys.insert("a", "q".to_string());
                transmit.to_keys(keys);
            }
            Self::TransmitDataAndDisplay {
                transmit,
                verbosity,
                placement,
            } => {
                keys.insert("a", "T".to_string());
                verbosity.to_keys(keys);
                placement.to_keys(keys);
                transmit.to_keys(keys);
            }
            Self::Display {
                image_id,
                image_number,
                placement,
                verbosity,
            } => {
                keys.insert("a", "p".to_string());
                verbosity.to_keys(keys);
                placement.to_keys(keys);
                if let Some(image_id) = image_id {
                    keys.insert("i", image_id.to_string());
                }
                if let Some(image_number) = image_number {
                    keys.insert("I", image_number.to_string());
                }
            }
            Self::Delete { what, verbosity } => {
                keys.insert("a", "d".to_string());
                verbosity.to_keys(keys);
                what.to_keys(keys);
            }
            Self::TransmitFrame {
                transmit,
                verbosity,
                frame,
            } => {
                keys.insert("a", "f".to_string());
                transmit.to_keys(keys);
                frame.to_keys(keys);
                verbosity.to_keys(keys);
            }
            Self::ComposeFrame { frame, verbosity } => {
                keys.insert("a", "c".to_string());
                frame.to_keys(keys);
                verbosity.to_keys(keys);
            }
        }
    }
}

impl Display for KittyImage {
    fn fmt(&self, f: &mut Formatter) -> Result<(), FmtError> {
        write!(f, "\x1b_G")?;
        let mut keys = BTreeMap::new();
        self.to_keys(&mut keys);
        let mut payload = None;
        let mut first = true;
        for (k, v) in keys {
            if k == "payload" {
                payload = Some(v);
            } else {
                if first {
                    first = false;
                } else {
                    write!(f, ",")?;
                }

                write!(f, "{}={}", k, v)?;
            }
        }

        if let Some(p) = payload {
            write!(f, ";{}", p)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use k9::assert_equal as assert_eq;

    #[test]
    fn kitty_payload() {
        assert_eq!(
            KittyImage::parse_apc("Gf=24,s=10,v=20;aGVsbG8=".as_bytes()).unwrap(),
            KittyImage::TransmitData {
                transmit: KittyImageTransmit {
                    format: Some(KittyImageFormat::Rgb),
                    data: KittyImageData::Direct("aGVsbG8=".to_string()),
                    width: Some(10),
                    height: Some(20),
                    image_id: None,
                    image_number: None,
                    compression: KittyImageCompression::None,
                    more_data_follows: false,
                },
                verbosity: KittyImageVerbosity::Verbose,
            }
        );

        assert_eq!(
            KittyImage::parse_apc("Ga=d,q=2".as_bytes()).unwrap(),
            KittyImage::Delete {
                what: KittyImageDelete::All { delete: false },
                verbosity: KittyImageVerbosity::Quiet
            }
        );

        assert_eq!(
            KittyImage::parse_apc(
                "Ga=f,x=119,y=384,s=17,v=32,i=7257421,X=1,r=1,q=2;AAAA=".as_bytes()
            )
            .unwrap(),
            KittyImage::TransmitFrame {
                transmit: KittyImageTransmit {
                    format: None,
                    data: KittyImageData::Direct("AAAA=".to_string()),
                    width: Some(17),
                    height: Some(32),
                    image_id: Some(7257421),
                    image_number: None,
                    compression: KittyImageCompression::None,
                    more_data_follows: false,
                },
                verbosity: KittyImageVerbosity::Quiet,
                frame: KittyImageFrame {
                    x: Some(119),
                    y: Some(384),
                    base_frame: None,
                    frame_number: Some(1),
                    composition_mode: KittyFrameCompositionMode::Overwrite,
                    background_pixel: None,
                    duration_ms: None,
                },
            }
        );
    }
}

/// The shared memory transport, against a real POSIX object.
///
/// A shared memory object can only be mapped on some platforms, so these go
/// through `load_data` rather than asserting on the parse, and cover the offset
/// and size a `t=s` transmission may carry.
#[cfg(all(test, feature = "kitty-shm", unix, not(target_os = "android")))]
mod shm_test {
    use super::*;
    use k9::assert_equal as assert_eq;
    use nix::fcntl::OFlag;
    use nix::sys::mman::{MapFlags, ProtFlags, mmap, munmap, shm_open, shm_unlink};
    use nix::sys::stat::Mode;
    use nix::unistd::ftruncate;
    use std::num::NonZeroUsize;

    /// Stage `data` in a shared memory object of that name, as a client sending
    /// `t=s` does.
    ///
    /// The name must fit macOS' `PSHMNAMLEN` of 31 bytes including the leading
    /// slash. A longer one fails `shm_open` with `ENAMETOOLONG`, which is not
    /// the failure these tests are looking for.
    fn stage(name: &str, data: &[u8]) -> String {
        assert!(
            name.len() <= 31,
            "{} is longer than macOS accepts for a shm name",
            name
        );

        // macOS refuses to resize an object that already has a size, so an
        // object left behind by an earlier run has to go first.
        let _ = shm_unlink(name);

        let fd = shm_open(
            name,
            OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .unwrap();
        ftruncate(&fd, data.len() as _).unwrap();

        let len = NonZeroUsize::new(data.len()).unwrap();
        // SAFETY: the object was created above and sized to `len`, so a mapping
        // of that length is backed for its whole extent.
        let ptr = unsafe {
            mmap(
                None,
                len,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &fd,
                0,
            )
        }
        .unwrap();
        // SAFETY: `ptr` maps `len` writable bytes and `data` is `len` long, so
        // the copy stays inside both, and the two cannot overlap because the
        // mapping was just created.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.as_ptr() as *mut u8, len.get());
            munmap(ptr, len.get()).unwrap();
        }

        name.to_string()
    }

    /// Whether an object of this name still exists.
    ///
    /// Only `ENOENT` counts as gone. Taking every error for absence would let a
    /// permission or name length problem pass as a successful unlink.
    fn present(name: &str) -> bool {
        match shm_open(name, OFlag::O_RDONLY, Mode::empty()) {
            Ok(_) => true,
            Err(nix::errno::Errno::ENOENT) => false,
            Err(err) => panic!("asking whether {} is still there answered {}", name, err),
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..=255u8).cycle().take(len).collect()
    }

    /// With neither `O` nor `S`, the whole object comes back.
    #[test]
    fn shm_reads_the_whole_object() {
        let data = pattern(4096);
        let name = stage("/wtshm-whole", &data);

        let got = KittyImageData::SharedMem {
            name: name.clone(),
            data_offset: None,
            data_size: None,
        }
        .load_data()
        .unwrap();

        // An object can report more than was staged in it, as Darwin rounds the
        // size up to a page; the data sits at the front.
        assert!(got.len() >= data.len());
        assert_eq!(&got[..data.len()], &data[..]);
        assert!(!present(&name), "the object is unlinked once it is read");
    }

    /// `O` and `S` select a range within the object. `O` is applied to the
    /// mapped bytes rather than to mmap, which only accepts a page aligned
    /// offset.
    #[test]
    fn shm_reads_the_slice_it_is_asked_for() {
        let data = pattern(4096);
        let name = stage("/wtshm-slice", &data);

        let got = KittyImageData::SharedMem {
            name: name.clone(),
            data_offset: Some(10),
            data_size: Some(80),
        }
        .load_data()
        .unwrap();

        assert_eq!(got, data[10..90].to_vec());
        assert!(!present(&name), "the object is unlinked once it is read");
    }

    /// An explicit size of nothing reads nothing. A client that means "all of
    /// it" omits `S`, which the parser also produces for `S=0`.
    #[test]
    fn shm_reads_nothing_for_a_size_of_nothing() {
        let name = stage("/wtshm-none", &pattern(64));

        let got = KittyImageData::SharedMem {
            name: name.clone(),
            data_offset: None,
            data_size: Some(0),
        }
        .load_data()
        .unwrap();

        assert_eq!(got, Vec::<u8>::new());
        assert!(!present(&name), "the object is unlinked once it is read");
    }

    /// An `O` beyond the object is refused rather than read, and the object is
    /// still unlinked so a rejected transmission leaves nothing behind.
    #[test]
    fn shm_refuses_an_offset_past_the_end() {
        let name = stage("/wtshm-oob", &pattern(64));

        let err = KittyImageData::SharedMem {
            name: name.clone(),
            data_offset: Some(1 << 20),
            data_size: Some(4),
        }
        .load_data()
        .unwrap_err();

        assert!(
            err.to_string().contains("offset"),
            "an offset past the end says so: {}",
            err
        );
        assert!(!present(&name), "a refused read still unlinks the object");
    }

    /// A name that does not exist names itself in the error, so a client that
    /// sent the wrong name, or one too long for the platform, can tell.
    #[test]
    fn shm_reports_an_object_that_is_not_there() {
        let _ = shm_unlink("/wtshm-absent");

        let err = KittyImageData::SharedMem {
            name: "/wtshm-absent".to_string(),
            data_offset: None,
            data_size: None,
        }
        .load_data()
        .unwrap_err();

        assert!(
            err.to_string().contains("/wtshm-absent"),
            "the error names the object: {}",
            err
        );
    }
}
