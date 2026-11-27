use anyhow::{anyhow, bail, ensure, Error};
use std::ffi::c_void;
use std::rc::Rc;

#[allow(non_camel_case_types, clippy::unreadable_literal)]
pub mod ffi {
    // gl_generator emits these weird cyclical and redundant type references;
    // the types appear to have to be in a module and need to reference super,
    // with some of the types specified in both scopes :-/
    pub mod generated {
        pub type khronos_utime_nanoseconds_t = super::khronos_utime_nanoseconds_t;
        pub type khronos_uint64_t = super::khronos_uint64_t;
        pub type khronos_ssize_t = super::khronos_ssize_t;
        pub type EGLNativeDisplayType = super::EGLNativeDisplayType;
        pub type EGLNativePixmapType = super::EGLNativePixmapType;
        pub type EGLNativeWindowType = super::EGLNativeWindowType;
        pub type EGLint = super::EGLint;
        pub type NativeDisplayType = super::EGLNativeDisplayType;
        pub type NativePixmapType = super::EGLNativePixmapType;
        pub type NativeWindowType = super::EGLNativeWindowType;

        include!(concat!(env!("OUT_DIR"), "/egl_bindings.rs"));
    }

    pub use generated::*;

    use std::os::raw;

    pub type EGLint = i32;
    pub type khronos_ssize_t = raw::c_long;
    pub type khronos_utime_nanoseconds_t = khronos_uint64_t;
    pub type khronos_uint64_t = u64;
    pub type EGLNativeDisplayType = *const raw::c_void;
    pub type EGLNativePixmapType = *const raw::c_void;

    #[cfg(target_os = "windows")]
    pub type EGLNativeWindowType = winapi::shared::windef::HWND;
    #[cfg(not(target_os = "windows"))]
    pub type EGLNativeWindowType = *const raw::c_void;
}

struct EglWrapper {
    _lib: libloading::Library,
    egl: ffi::Egl,
}

impl std::fmt::Debug for EglWrapper {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("EglWrapper").finish()
    }
}

/// EGL_NV_robustness_video_memory_purge, which the generated bindings lack
const GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV: u32 = 0x334C;

#[derive(Debug)]
pub struct GlConnection {
    egl: EglWrapper,
    display: ffi::types::EGLDisplay,
    is_opengl: bool,
    extensions: String,
}

impl GlConnection {
    #[allow(dead_code)]
    pub fn has_extension(&self, wanted: &str) -> bool {
        self.extensions.split(' ').any(|ext| ext == wanted)
    }
}

impl std::ops::Deref for GlConnection {
    type Target = ffi::Egl;

    fn deref(&self) -> &ffi::Egl {
        &self.egl.egl
    }
}

impl Drop for GlConnection {
    fn drop(&mut self) {
        unsafe {
            self.egl.egl.Terminate(self.display);
        }
    }
}

thread_local! {
    /// The last make_current of this thread failed; glium's reset query then
    /// reports a reset
    static MAKE_CURRENT_FAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The names glium loads the reset-status query under; it calls the one its
/// context supports, so each keeps its own driver function. glium 0.35 never
/// calls the KHR one; wrapped all the same, for a glium that does.
const RESET_STATUS_NAMES: [&str; 4] = [
    "glGetGraphicsResetStatus",
    "glGetGraphicsResetStatusARB",
    "glGetGraphicsResetStatusEXT",
    "glGetGraphicsResetStatusKHR",
];
const RESET_STATUS_CORE: usize = 0;
const RESET_STATUS_ARB: usize = 1;
const RESET_STATUS_EXT: usize = 2;
const RESET_STATUS_KHR: usize = 3;

static DRIVER_RESET_STATUS: [std::sync::atomic::AtomicPtr<c_void>; 4] =
    [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; 4];

fn driver_reset_status(name: usize) -> u32 {
    let f = DRIVER_RESET_STATUS[name].load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        return 0;
    }
    unsafe { std::mem::transmute::<*mut c_void, extern "system" fn() -> u32>(f)() }
}

/// glium's make_current cannot report a failure, and with no context current
/// the driver reports no reset, so the frame went on into the null pointer
/// glMapBufferRange returned. glium's query answers lost instead.
/// The reason is logged here: glium asks with the variant its context
/// supports, and stops asking once it saw the context lost.
fn reset_status(name: usize) -> u32 {
    if MAKE_CURRENT_FAILED.with(|failed| failed.get()) {
        return 0x8255; // GL_UNKNOWN_CONTEXT_RESET
    }
    let status = driver_reset_status(name);
    if status != 0 {
        let reason = match status {
            0x8253 => "guilty",
            0x8254 => "innocent",
            0x8255 => "unknown cause",
            0x92BB => "video memory purged",
            _ => "unrecognized status",
        };
        // NVIDIA reports "guilty" for the work it drops while another
        // session has the GPU
        log::warn!("the driver reset the opengl context ({reason}, {status:#x})");
    }
    status
}

extern "system" fn reset_status_core() -> u32 {
    reset_status(RESET_STATUS_CORE)
}
extern "system" fn reset_status_arb() -> u32 {
    reset_status(RESET_STATUS_ARB)
}
extern "system" fn reset_status_ext() -> u32 {
    reset_status(RESET_STATUS_EXT)
}
extern "system" fn reset_status_khr() -> u32 {
    reset_status(RESET_STATUS_KHR)
}
const RESET_STATUS_WRAPPERS: [extern "system" fn() -> u32; 4] = [
    reset_status_core,
    reset_status_arb,
    reset_status_ext,
    reset_status_khr,
];

#[derive(Debug)]
pub struct GlState {
    connection: Rc<GlConnection>,
    surface: ffi::types::EGLSurface,
    context: ffi::types::EGLContext,
    after_drop: AfterDrop,
}

/// Runs once the surface and context are destroyed: the native window
/// they were made on must outlive them.
#[derive(Default)]
struct AfterDrop(std::cell::RefCell<Option<Box<dyn FnOnce()>>>);

impl std::fmt::Debug for AfterDrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AfterDrop")
    }
}

impl Drop for GlState {
    fn drop(&mut self) {
        unsafe {
            self.connection.MakeCurrent(
                self.connection.display,
                ffi::NO_SURFACE,
                ffi::NO_SURFACE,
                ffi::NO_CONTEXT,
            );
            self.connection
                .DestroySurface(self.connection.display, self.surface);
            self.connection
                .DestroyContext(self.connection.display, self.context);
        }
        if let Some(after_drop) = self.after_drop.0.borrow_mut().take() {
            after_drop();
        }
    }
}

type GetProcAddressFunc =
    unsafe extern "C" fn(*const std::os::raw::c_char) -> *const std::os::raw::c_void;

impl EglWrapper {
    pub fn load_egl(lib: libloading::Library) -> anyhow::Result<Self> {
        let get_proc_address: libloading::Symbol<GetProcAddressFunc> =
            unsafe { lib.get(b"eglGetProcAddress\0")? };
        let egl = ffi::Egl::load_with(|s: &'static str| {
            let sym_name = std::ffi::CString::new(s).expect("symbol to be cstring compatible");
            if let Ok(sym) = unsafe { lib.get(sym_name.as_bytes_with_nul()) } {
                return *sym;
            }
            unsafe { get_proc_address(sym_name.as_ptr()) }
        });
        log::trace!("load_egl: {:?}", lib);
        Ok(Self { _lib: lib, egl })
    }

    fn get_display(
        &self,
        display: Option<ffi::EGLNativeDisplayType>,
    ) -> anyhow::Result<ffi::types::EGLDisplay> {
        let display = unsafe { self.egl.GetDisplay(display.unwrap_or(ffi::DEFAULT_DISPLAY)) };
        if display.is_null() {
            Err(self.error("egl GetDisplay"))
        } else {
            Ok(display)
        }
    }

    pub fn error(&self, context: &str) -> Error {
        let label = match unsafe { self.egl.GetError() } as u32 {
            ffi::NOT_INITIALIZED => "NOT_INITIALIZED".into(),
            ffi::BAD_ACCESS => "BAD_ACCESS".into(),
            ffi::BAD_ALLOC => "BAD_ALLOC".into(),
            ffi::BAD_ATTRIBUTE => "BAD_ATTRIBUTE".into(),
            ffi::BAD_CONTEXT => "BAD_CONTEXT".into(),
            ffi::BAD_CURRENT_SURFACE => "BAD_CURRENT_SURFACE".into(),
            ffi::BAD_DISPLAY => "BAD_DISPLAY".into(),
            ffi::BAD_SURFACE => "BAD_SURFACE".into(),
            ffi::BAD_MATCH => "BAD_MATCH".into(),
            ffi::BAD_PARAMETER => "BAD_PARAMETER".into(),
            ffi::BAD_NATIVE_PIXMAP => "BAD_NATIVE_PIXMAP".into(),
            ffi::BAD_NATIVE_WINDOW => "BAD_NATIVE_WINDOW".into(),
            ffi::CONTEXT_LOST => "CONTEXT_LOST".into(),
            ffi::SUCCESS => "Failed but with error code: SUCCESS".into(),
            err => format!("EGL Error code: {}", err),
        };
        anyhow!("{}: {}", context, label)
    }

    pub fn initialize_and_get_version(
        &self,
        display: ffi::types::EGLDisplay,
    ) -> anyhow::Result<(ffi::EGLint, ffi::EGLint)> {
        let mut major = 0;
        let mut minor = 0;
        unsafe {
            if self.egl.Initialize(display, &mut major, &mut minor) != 0 {
                Ok((major, minor))
            } else {
                Err(self.error("egl Initialize"))
            }
        }
    }

    fn config_attrib(
        &self,
        display: ffi::types::EGLDisplay,
        config: ffi::types::EGLConfig,
        attribute: u32,
    ) -> Option<ffi::EGLint> {
        let mut value = 0;
        let res = unsafe {
            self.egl
                .GetConfigAttrib(display, config, attribute as ffi::EGLint, &mut value)
        };
        if res == 1 {
            Some(value)
        } else {
            None
        }
    }

    fn log_config_info(&self, display: ffi::types::EGLDisplay, config: ffi::types::EGLConfig) {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct ConfigInfo {
            config: ffi::types::EGLConfig,
            alpha_size: Option<ffi::EGLint>,
            red_size: Option<ffi::EGLint>,
            green_size: Option<ffi::EGLint>,
            blue_size: Option<ffi::EGLint>,
            depth_size: Option<ffi::EGLint>,
            conformant: Option<String>,
            renderable_type: Option<String>,
            native_visual_id: Option<ffi::EGLint>,
            surface_type: Option<String>,
        }

        fn conformant_bits(bits: ffi::EGLint) -> String {
            let bits = bits as ffi::types::EGLenum;
            let mut s = String::new();
            if bits & ffi::OPENGL_BIT != 0 {
                s.push_str("OPENGL ");
            }
            if bits & ffi::OPENGL_ES2_BIT != 0 {
                s.push_str("OPENGL_ES2 ");
            }
            if bits & ffi::OPENGL_ES3_BIT != 0 {
                s.push_str("OPENGL_ES3 ");
            }
            if bits & ffi::OPENVG_BIT != 0 {
                s.push_str("OPENVG_BIT ");
            }
            s
        }

        fn surface_bits(bits: ffi::EGLint) -> String {
            let bits = bits as ffi::types::EGLenum;
            let mut s = String::new();
            if bits & ffi::PBUFFER_BIT != 0 {
                s.push_str("PBUFFER ");
            }
            if bits & ffi::PIXMAP_BIT != 0 {
                s.push_str("PIXMAP ");
            }
            if bits & ffi::WINDOW_BIT != 0 {
                s.push_str("WINDOW ");
            }
            s
        }

        let info = ConfigInfo {
            config,
            alpha_size: self.config_attrib(display, config, ffi::ALPHA_SIZE),
            red_size: self.config_attrib(display, config, ffi::RED_SIZE),
            green_size: self.config_attrib(display, config, ffi::GREEN_SIZE),
            blue_size: self.config_attrib(display, config, ffi::BLUE_SIZE),
            depth_size: self.config_attrib(display, config, ffi::DEPTH_SIZE),
            conformant: self
                .config_attrib(display, config, ffi::CONFORMANT)
                .map(conformant_bits),
            renderable_type: self
                .config_attrib(display, config, ffi::RENDERABLE_TYPE)
                .map(conformant_bits),
            native_visual_id: self.config_attrib(display, config, ffi::NATIVE_VISUAL_ID),
            surface_type: self
                .config_attrib(display, config, ffi::SURFACE_TYPE)
                .map(surface_bits),
        };

        log::trace!("{:x?}", info);
    }

    pub fn choose_config(
        &self,
        display: ffi::types::EGLDisplay,
        attributes: &[u32],
    ) -> anyhow::Result<Vec<ffi::types::EGLConfig>> {
        ensure!(
            !attributes.is_empty() && attributes[attributes.len() - 1] == ffi::NONE,
            "attributes list must be terminated with ffi::NONE"
        );

        let mut num_configs = 0;
        if unsafe {
            self.egl
                .GetConfigs(display, std::ptr::null_mut(), 0, &mut num_configs)
        } != 1
        {
            return Err(self.error("egl GetConfigs to count possible number of configurations"));
        }

        let mut configs = vec![std::ptr::null(); num_configs as usize];

        if unsafe {
            self.egl
                .GetConfigs(display, configs.as_mut_ptr(), num_configs, &mut num_configs)
        } != 1
        {
            return Err(self.error("egl GetConfigs to enumerate configurations"));
        }

        log::trace!("Available Configuration(s):");
        for c in &configs {
            self.log_config_info(display, *c);
        }

        if unsafe {
            self.egl.ChooseConfig(
                display,
                attributes.as_ptr() as *const ffi::EGLint,
                configs.as_mut_ptr(),
                configs.len() as ffi::EGLint,
                &mut num_configs,
            )
        } != 1
        {
            return Err(self.error("egl ChooseConfig to select configurations"));
        }

        configs.resize(num_configs as usize, std::ptr::null());

        log::trace!("Matching Configuration(s):");
        for c in &configs {
            self.log_config_info(display, *c);
        }

        // If we're running on a system with 30bpp color depth then ChooseConfig
        // will bias towards putting 10bpc matches first, but we want 8-bit.
        // Let's filter out matches that are too deep
        configs.retain(|config| {
            self.config_attrib(display, *config, ffi::RED_SIZE) == Some(8)
                && self.config_attrib(display, *config, ffi::GREEN_SIZE) == Some(8)
                && self.config_attrib(display, *config, ffi::BLUE_SIZE) == Some(8)
        });

        // Sort by descending alpha size, otherwise we can end up selecting
        // alpha size 0 under XWayland, even though a superior config with
        // 32bpp 8bpc is available.  For whatever reason (probably a Wayland/mutter
        // weirdness) that renders with a transparent background on my pixelbook.
        configs.sort_by(|a, b| {
            self.config_attrib(display, *a, ffi::ALPHA_SIZE)
                .cmp(&self.config_attrib(display, *b, ffi::ALPHA_SIZE))
                .reverse()
        });

        log::trace!("Filtered down to these configuration(s):");
        for c in &configs {
            self.log_config_info(display, *c);
        }

        Ok(configs)
    }

    pub fn create_window_surface(
        &self,
        display: ffi::types::EGLDisplay,
        config: ffi::types::EGLConfig,
        window: ffi::EGLNativeWindowType,
    ) -> anyhow::Result<ffi::types::EGLSurface> {
        let surface = unsafe {
            self.egl.CreateWindowSurface(
                display,
                config,
                window,
                [
                    ffi::GL_COLORSPACE as i32,
                    ffi::GL_COLORSPACE_SRGB as i32,
                    ffi::NONE as i32,
                ]
                .as_ptr(),
            )
        };
        if surface.is_null() {
            Err(self.error("EGL CreateWindowSurface"))
        } else {
            Ok(surface)
        }
    }

    pub fn create_context(
        &self,
        display: ffi::types::EGLDisplay,
        config: ffi::types::EGLConfig,
        share_context: ffi::types::EGLContext,
        attributes: &[u32],
    ) -> anyhow::Result<ffi::types::EGLConfig> {
        ensure!(
            !attributes.is_empty() && attributes[attributes.len() - 1] == ffi::NONE,
            "attributes list must be terminated with ffi::NONE"
        );
        let context = unsafe {
            self.egl.CreateContext(
                display,
                config,
                share_context,
                attributes.as_ptr() as *const i32,
            )
        };
        if context.is_null() {
            Err(self.error("EGL CreateContext"))
        } else {
            Ok(context)
        }
    }
}

impl GlState {
    #[cfg_attr(any(windows, target_os = "macos"), allow(unused))]
    pub fn get_connection(&self) -> &Rc<GlConnection> {
        &self.connection
    }

    /// Called after this state's surface and context are destroyed.
    pub fn set_after_drop(&self, f: impl FnOnce() + 'static) {
        self.after_drop.0.borrow_mut().replace(Box::new(f));
    }

    fn with_egl_lib<F: FnMut(EglWrapper) -> anyhow::Result<Self>>(
        mut func: F,
    ) -> anyhow::Result<Self> {
        let mut paths: Vec<std::path::PathBuf> = vec![
            #[cfg(target_os = "windows")]
            "libEGL.dll".into(),
            #[cfg(target_os = "windows")]
            "atioglxx.dll".into(),
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            "libEGL.so.1".into(),
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            "libEGL.so".into(),
        ];

        if cfg!(target_os = "macos") {
            // On macOS, let's look in the application directory to see
            // if we've deployed libEGL.dylib alongside; if so, we want
            // to try loading that.
            paths.push(
                std::env::current_exe()?
                    .parent()
                    .ok_or_else(|| anyhow!("current_exe isn't in a directory!?"))?
                    .join("libEGL.dylib"),
            );

            // And just in case, let's also allow loading via
            // DYLD_LIBRARY_PATH
            paths.push("libEGL.dylib".into());
        }

        let mut errors = vec![];
        let mut prefer_swrast = crate::configuration::prefer_swrast();

        for _ in 0..2 {
            if prefer_swrast {
                // Assuming that we're using Mesa, set an environment
                // variable that should select CPU based rendering.
                std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "true");
            }
            for path in &paths {
                match unsafe { libloading::Library::new(path) } {
                    Ok(lib) => match EglWrapper::load_egl(lib) {
                        Ok(egl) => match func(egl) {
                            Ok(result) => {
                                return Ok(result);
                            }
                            Err(e) => {
                                errors.push(format!(
                                    "with_egl_lib({}) failed: {}",
                                    path.display(),
                                    e
                                ));
                            }
                        },
                        Err(e) => {
                            errors.push(format!("load_egl {} failed: {}", path.display(), e));
                        }
                    },
                    Err(e) => {
                        errors.push(format!("{}: {}", path.display(), e));
                    }
                }
            }
            // Since we didn't yet succeed, try enabling software rasterization.
            // However, don't do this on Windows; the EGL implementation on
            // Windows isn't MESA so there's no point trying a second pass
            // with the mesa environment set, and if we did, it would just
            // cause us to try software mode instead of the native opengl
            // drivers we'd pick up from the WGL fallback.
            if cfg!(windows) || cfg!(target_os = "macos") {
                break;
            }
            if prefer_swrast {
                break;
            }
            prefer_swrast = true;
        }
        bail!("with_egl_lib failed: {}", errors.join(", "))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[cfg(feature = "wayland")]
    pub fn create_wayland(
        display: Option<ffi::EGLNativeDisplayType>,
        wegl_surface: &wayland_egl::WlEglSurface,
    ) -> anyhow::Result<Self> {
        Self::create(display, wegl_surface.ptr())
    }

    pub fn create(
        display: Option<ffi::EGLNativeDisplayType>,
        window: ffi::EGLNativeWindowType,
    ) -> anyhow::Result<Self> {
        Self::with_egl_lib(|egl| {
            let egl_display = egl.get_display(display)?;

            let (major, minor) = egl.initialize_and_get_version(egl_display)?;
            log::trace!("initialized EGL version {}.{}", major, minor);

            let is_opengl = unsafe {
                if egl.egl.BindAPI(ffi::OPENGL_API) != 0 {
                    log::trace!("using OpenGL");
                    true
                } else if egl.egl.BindAPI(ffi::OPENGL_ES_API) != 0 {
                    log::trace!("using GLES");
                    false
                } else {
                    anyhow::bail!("Unable to bind to OpenGL or GL ES!?");
                }
            };

            let extensions = unsafe { egl.egl.QueryString(egl_display, ffi::EXTENSIONS as _) };
            let extensions = if extensions.is_null() {
                String::new()
            } else {
                let cstr = unsafe { std::ffi::CStr::from_ptr(extensions) };
                String::from_utf8_lossy(cstr.to_bytes()).to_string()
            };
            log::trace!("EGL extensions: {}", extensions);

            let connection = Rc::new(GlConnection {
                display: egl_display,
                egl,
                is_opengl,
                extensions,
            });

            Self::create_with_existing_connection(&connection, window)
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[cfg(feature = "wayland")]
    pub fn create_wayland_with_existing_connection(
        connection: &Rc<GlConnection>,
        wegl_surface: &wayland_egl::WlEglSurface,
    ) -> anyhow::Result<Self> {
        Self::create_with_existing_connection(connection, wegl_surface.ptr())
    }

    pub fn create_with_existing_connection(
        connection: &Rc<GlConnection>,
        window: ffi::EGLNativeWindowType,
    ) -> anyhow::Result<GlState> {
        let configs = connection.egl.choose_config(
            connection.display,
            &[
                // We're explicitly asking for any alpha size; this is
                // the default behavior but we're making it explicit here
                // for the sake of documenting our intent.
                // In general we want 32bpp with 8bpc, but for displays
                // that are natively 10bpc we should be fine with relaxing
                // this to 0 alpha bits, so by asking for 0 here we effectively
                // indicate that we don't care.
                // In our implementation of choose_config we will return
                // only entries with 8bpc for red/green/blue so we should
                // end up with either 24bpp/8bpc with no alpha, or 32bpp/8bpc
                // with 8bpc alpha.
                ffi::ALPHA_SIZE,
                0,
                // Request at least 8bpc, 24bpp.  The implementation may
                // return a context capable of more than this.
                ffi::RED_SIZE,
                8,
                ffi::GREEN_SIZE,
                8,
                ffi::BLUE_SIZE,
                8,
                ffi::DEPTH_SIZE,
                24,
                ffi::CONFORMANT,
                if connection.is_opengl {
                    ffi::OPENGL_BIT
                } else {
                    ffi::OPENGL_ES3_BIT
                },
                ffi::RENDERABLE_TYPE,
                if connection.is_opengl {
                    ffi::OPENGL_BIT
                } else {
                    ffi::OPENGL_ES3_BIT
                },
                // Wayland EGL doesn't give us a working context if we request
                // PBUFFER|PIXMAP.  We don't appear to require these for X11,
                // so we're just asking for a WINDOW capable context
                ffi::SURFACE_TYPE,
                ffi::WINDOW_BIT, //| ffi::PBUFFER_BIT | ffi::PIXMAP_BIT,
                ffi::NONE,
            ],
        )?;

        if configs.is_empty() {
            anyhow::bail!("no compatible EGL configuration was found");
        }
        let mut errors = String::new();

        for config in configs {
            let surface =
                match connection
                    .egl
                    .create_window_surface(connection.display, config, window)
                {
                    Ok(s) => s,
                    Err(e) => {
                        errors.push_str(&format!("{:#} {:x?}\n", e, config));
                        continue;
                    }
                };

            let mut attempts = vec![];
            if cfg!(windows) {
                // On Windows, where drivers may be dynamically unloaded,
                // let's make an effort to try to survive that event.
                attempts.push(vec![
                    ffi::CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
                    ffi::TRUE,
                    ffi::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
                    ffi::LOSE_CONTEXT_ON_RESET_EXT,
                ]);
            } else {
                // Reset notification, where offered: NVIDIA resets the contexts
                // of a session on another console, and its purge notification
                // reports the video memory it takes from them, which otherwise
                // came back garbled. Each attempt asks for less.
                let (strategy, extension) = if connection.is_opengl {
                    (
                        [
                            ffi::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR,
                            ffi::LOSE_CONTEXT_ON_RESET_KHR,
                        ],
                        "EGL_KHR_create_context",
                    )
                } else {
                    (
                        [
                            ffi::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
                            ffi::LOSE_CONTEXT_ON_RESET_EXT,
                        ],
                        "EGL_EXT_create_context_robustness",
                    )
                };
                if connection.has_extension(extension) {
                    if connection.has_extension("EGL_NV_robustness_video_memory_purge") {
                        let mut purge = strategy.to_vec();
                        purge.extend([GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV, ffi::TRUE]);
                        attempts.push(purge);
                    }
                    attempts.push(strategy.to_vec());
                }
                attempts.push(vec![]);
            }

            let mut context = None;
            for extra in attempts {
                let mut attributes = vec![ffi::CONTEXT_MAJOR_VERSION, 3];
                attributes.extend(&extra);
                attributes.push(ffi::NONE);
                match connection.egl.create_context(
                    connection.display,
                    config,
                    std::ptr::null(),
                    &attributes,
                ) {
                    Ok(c) => {
                        log::debug!("EGL context attributes {:x?}", attributes);
                        context = Some(c);
                        break;
                    }
                    Err(e) => {
                        log::debug!("EGL context attributes {:x?} refused: {:#}", attributes, e);
                        errors.push_str(&format!("{:#} {:x?} {:x?}\n", e, config, attributes));
                    }
                }
            }
            let context = match context {
                Some(c) => c,
                None => continue,
            };

            // Current once here, where a failure is an error: glium makes it
            // current without checking, then reads the GL version through a
            // null pointer.
            if unsafe { connection.MakeCurrent(connection.display, surface, surface, context) } == 0
            {
                let e = connection.egl.error("MakeCurrent");
                errors.push_str(&format!("{:#} {:x?}\n", e, config));
                unsafe {
                    connection.DestroyContext(connection.display, context);
                    connection.DestroySurface(connection.display, surface);
                }
                continue;
            }

            log::trace!("Successfully created a surface using this configuration");
            connection.egl.log_config_info(connection.display, config);

            // Request non-blocking buffer swaps; we'll manage throttling
            // frames at the application level.
            unsafe {
                connection.egl.egl.SwapInterval(connection.display, 0);
            }

            return Ok(Self {
                connection: Rc::clone(connection),
                context,
                surface,
                after_drop: AfterDrop::default(),
            });
        }

        Err(anyhow!(errors))
    }
}

unsafe impl glium::backend::Backend for GlState {
    fn resize(&self, _: (u32, u32)) {
        todo!()
    }

    fn swap_buffers(&self) -> Result<(), glium::SwapBuffersError> {
        let res = unsafe {
            self.connection
                .SwapBuffers(self.connection.display, self.surface)
        };
        if res != 1 {
            Err(match unsafe { self.connection.GetError() } as u32 {
                ffi::CONTEXT_LOST => glium::SwapBuffersError::ContextLost,
                _ => glium::SwapBuffersError::AlreadySwapped,
            })
        } else {
            Ok(())
        }
    }

    unsafe fn get_proc_address(&self, symbol: &str) -> *const c_void {
        let sym_name = std::ffi::CString::new(symbol).expect("symbol to be cstring compatible");
        let f = self.connection.GetProcAddress(sym_name.as_ptr()) as *const c_void;
        if let Some(name) = RESET_STATUS_NAMES.iter().position(|&n| n == symbol) {
            if !f.is_null() {
                DRIVER_RESET_STATUS[name]
                    .store(f as *mut c_void, std::sync::atomic::Ordering::Relaxed);
                return RESET_STATUS_WRAPPERS[name] as *const c_void;
            }
        }
        f
    }

    fn get_framebuffer_dimensions(&self) -> (u32, u32) {
        let mut width = 0;
        let mut height = 0;

        unsafe {
            self.connection.QuerySurface(
                self.connection.display,
                self.surface,
                ffi::WIDTH as i32,
                &mut width,
            );
        }
        unsafe {
            self.connection.QuerySurface(
                self.connection.display,
                self.surface,
                ffi::HEIGHT as i32,
                &mut height,
            );
        }
        (width as u32, height as u32)
    }

    fn is_current(&self) -> bool {
        unsafe { self.connection.GetCurrentContext() == self.context }
    }

    unsafe fn make_current(&self) {
        let failed = self.connection.MakeCurrent(
            self.connection.display,
            self.surface,
            self.surface,
            self.context,
        ) == 0;
        if failed {
            // once per run of failures: glium retries on every GL call
            if !MAKE_CURRENT_FAILED.with(|f| f.get()) {
                let err = self.connection.egl.error("MakeCurrent");
                log::error!(
                    "make_current failed for surface {:?}, context {:?}: {:#}",
                    self.surface,
                    self.context,
                    err
                );
            }
            // whatever stayed current belongs to another window
            self.connection.MakeCurrent(
                self.connection.display,
                ffi::NO_SURFACE,
                ffi::NO_SURFACE,
                ffi::NO_CONTEXT,
            );
        }
        // Set even if the release failed too and another window's context is
        // still current: that one may then be reported lost (recreated, or
        // its window closed where recovery is unsupported), which is better
        // than this window's GL calls reaching it.
        MAKE_CURRENT_FAILED.with(|f| f.set(failed));
    }
}
