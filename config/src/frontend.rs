use luahelper::impl_lua_conversion_dynamic;
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic, Default)]
pub enum FrontEndSelection {
    #[default]
    OpenGL,
    WebGpu,
    Software,
    /// Windows-only GDI text renderer (ExtTextOutW direct-to-HDC).
    /// On non-Windows platforms this falls back to OpenGL with a warning.
    Gdi,
}

impl FrontEndSelection {
    /// Returns true for the GDI text renderer, which is only meaningful on Windows.
    pub fn is_gdi(&self) -> bool {
        matches!(self, FrontEndSelection::Gdi)
    }

    /// Resolve the selection for the current platform. The GDI renderer is
    /// Windows-only; if it is somehow selected on another platform we fall back
    /// to OpenGL with a logged warning.
    pub fn resolve(self) -> FrontEndSelection {
        #[cfg(not(windows))]
        {
            if matches!(self, FrontEndSelection::Gdi) {
                log::warn!(
                    "front_end=\"Gdi\" is only supported on Windows; falling back to OpenGL"
                );
                return FrontEndSelection::OpenGL;
            }
        }
        self
    }
}

/// Returns true if we are running in an RDP session.
/// Mirrors `window::os::windows::is_running_in_rdp_session`, duplicated here
/// because the `config` crate cannot depend on the `window` crate.
/// See <https://docs.microsoft.com/en-us/windows/win32/termserv/detecting-the-terminal-services-environment>
#[cfg(windows)]
fn is_running_in_rdp_session() -> bool {
    use winapi::shared::minwindef::DWORD;
    use winapi::um::processthreadsapi::{GetCurrentProcessId, ProcessIdToSessionId};
    use winapi::um::winuser::{GetSystemMetrics, SM_REMOTESESSION};
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    if unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0 {
        return true;
    }

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let terminal_server =
        match hklm.open_subkey("SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\") {
            Ok(k) => k,
            Err(_) => return false,
        };

    let glass_session_id: DWORD = match terminal_server.get_value("GlassSessionId") {
        Ok(sess) => sess,
        Err(_) => return false,
    };

    unsafe {
        let mut current_session = 0;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session) != 0 {
            current_session != glass_session_id
        } else {
            false
        }
    }
}

/// Default value for `front_end` when the user has not set it explicitly.
///
/// On Windows, when running inside an RDP session we default to `Software`
/// rendering (a mature, full-feature path), matching the historical behavior.
/// The GDI text renderer (`Gdi`) remotes output as text and is much lighter over
/// RDP, but is an MVP that does not yet render overlay/modal UI, so it is
/// opt-in rather than auto-selected. An explicit `front_end` always wins because
/// this function is only consulted when the field is unset.
pub fn default_front_end() -> FrontEndSelection {
    #[cfg(windows)]
    {
        if is_running_in_rdp_session() {
            log::trace!("Running in an RDP session, defaulting front_end to Software");
            return FrontEndSelection::Software;
        }
    }
    FrontEndSelection::OpenGL
}

/// Corresponds to <https://docs.rs/wgpu/latest/wgpu/struct.AdapterInfo.html>
#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct GpuInfo {
    pub name: String,
    pub device_type: String,
    pub backend: String,
    pub driver: Option<String>,
    pub driver_info: Option<String>,
    pub vendor: Option<u32>,
    pub device: Option<u32>,
}
impl_lua_conversion_dynamic!(GpuInfo);

impl ToString for GpuInfo {
    fn to_string(&self) -> String {
        let mut result = format!(
            "name={}, device_type={}, backend={}",
            self.name, self.device_type, self.backend
        );
        if let Some(driver) = &self.driver {
            result.push_str(&format!(", driver={driver}"));
        }
        if let Some(driver_info) = &self.driver_info {
            result.push_str(&format!(", driver_info={driver_info}"));
        }
        if let Some(vendor) = &self.vendor {
            result.push_str(&format!(", vendor={vendor}"));
        }
        if let Some(device) = &self.device {
            result.push_str(&format!(", device={device}"));
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum WebGpuPowerPreference {
    LowPower,
    HighPerformance,
}

impl Default for WebGpuPowerPreference {
    fn default() -> Self {
        Self::LowPower
    }
}
