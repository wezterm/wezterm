//! COM class implementing `ITerminalHandoff3` + `IDefaultTerminalMarker`.
//! Single-vtable pattern: the same `Instance` satisfies all three IIDs via QI.

// Vtable field names (`QueryInterface`, `AddRef`, `Release`, …) and the
// `This` parameter name are mandated by the COM ABI / MIDL convention.
#![allow(non_snake_case)]

use std::sync::OnceLock;

use winapi::shared::ntdef::HANDLE;

mod establish;
mod factory;
mod instance;

pub(crate) use factory::take_factory;
#[allow(unused_imports)]
pub(crate) use instance::take_singleton;

/// Owned version of `TerminalStartupInfo`. `width` / `height` come from
/// the IDL's `dwXSize` / `dwYSize`; consumers MUST treat 0 as
/// "unspecified" (MS conhost leaves them zero on the wire).
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct TerminalStartupInfoOwned {
    pub title: Option<String>,
    pub icon_path: Option<String>,
    pub icon_index: i32,
    pub show_window: u16,
    pub width: u16,
    pub height: u16,
}

/// Per `ITerminalHandoff3::EstablishPtyHandoff` (IDL lines 75-76),
/// `in_handle` and `out_handle` are `[out] HANDLE*` — WezTerm allocates
/// the ConPTY pipes and writes the ConPTY-side ends through them.
///
/// `signal`, `reference`, `server`, `client` are `[in]` and owned by
/// the COM runtime (the proxy stub closes them after the call returns).
pub type HandoffCallback = Box<
    dyn Fn(
            *mut HANDLE,
            *mut HANDLE,
            HANDLE,
            HANDLE,
            HANDLE,
            HANDLE,
            TerminalStartupInfoOwned,
        ) -> anyhow::Result<()>
        + Send
        + Sync,
>;

/// Mirrors `CTerminalHandoff::s_setCallback`
/// (`microsoft/terminal/src/cascadia/TerminalConnection/CTerminalHandoff.cpp:19-22`).
static HANDOFF_CALLBACK: OnceLock<HandoffCallback> = OnceLock::new();

pub fn set_callback(callback: HandoffCallback) -> Result<(), HandoffCallback> {
    HANDOFF_CALLBACK.set(callback)
}
