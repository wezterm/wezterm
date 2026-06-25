//! COM interface GUIDs matching `microsoft/terminal`'s
//! `src/host/proxy/IConsoleHandoff.idl` and `ITerminalHandoff.idl`.
//!
//! Defined as raw `#[repr(C)]` vtables because the IDL uses
//! `[system_handle(sh_*)]` params that `winapi` doesn't model.

#![allow(non_upper_case_globals)]

use winapi::shared::guiddef::GUID;

// `{00000000-0000-0000-C000-000000000046}` — IUnknown IID (unknwnbase.h).
// Must be the canonical value, not GUID_NULL: once an IClassFactory is
// registered, CoCreateInstance's internal QI chain matches against it.
// The previous all-zeros value silently broke QI.
pub const IID_IUNKNOWN: GUID = GUID {
    Data1: 0x00000000,
    Data2: 0x0000,
    Data3: 0x0000,
    Data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

// `{00000001-0000-0000-C000-000000000046}` — IClassFactory IID.
pub const IID_IClassFactory: GUID = GUID {
    Data1: 0x00000001,
    Data2: 0x0000,
    Data3: 0x0000,
    Data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

/// `{59D55CCE-FC8A-48B4-ACE8-0A9286C6557F}`
pub const IID_ITerminalHandoff: GUID = GUID {
    Data1: 0x59D55CCE,
    Data2: 0xFC8A,
    Data3: 0x48B4,
    Data4: [0xAC, 0xE8, 0x0A, 0x92, 0x86, 0xC6, 0x55, 0x7F],
};

/// `{AA6B364F-4A50-4176-9002-0AE755E7B5EF}`
pub const IID_ITerminalHandoff2: GUID = GUID {
    Data1: 0xAA6B364F,
    Data2: 0x4A50,
    Data3: 0x4176,
    Data4: [0x90, 0x02, 0x0A, 0xE7, 0x55, 0xE7, 0xB5, 0xEF],
};

/// `{6F23DA90-15C5-4203-9DB0-64E73F1B1B00}`
pub const IID_ITerminalHandoff3: GUID = GUID {
    Data1: 0x6F23DA90,
    Data2: 0x15C5,
    Data3: 0x4203,
    Data4: [0x9D, 0xB0, 0x64, 0xE7, 0x3F, 0x1B, 0x1B, 0x00],
};

/// `{746E6BC0-AB05-4E38-AB14-71E86763141F}`
pub const IID_IDefaultTerminalMarker: GUID = GUID {
    Data1: 0x746E6BC0,
    Data2: 0xAB05,
    Data3: 0x4E38,
    Data4: [0xAB, 0x14, 0x71, 0xE8, 0x67, 0x63, 0x14, 0x1F],
};

/// `{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}` — WezTerm's termhost terminal CLSID.
pub const CLSID_WezTermTerminalHandoff: GUID = GUID {
    Data1: 0x8B7D4E2A,
    Data2: 0x3F5C,
    Data3: 0x4D1B,
    Data4: [0x9A, 0x6E, 0x7C, 0x2B, 0x5F, 0x8D, 0x1E, 0x4A],
};

#[cfg(test)]
pub fn guid_to_string(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        g.Data1,
        g.Data2,
        g.Data3,
        ((g.Data4[0] as u16) << 8) | (g.Data4[1] as u16),
        ((g.Data4[2] as u64) << 40)
            | ((g.Data4[3] as u64) << 32)
            | ((g.Data4[4] as u64) << 24)
            | ((g.Data4[5] as u64) << 16)
            | ((g.Data4[6] as u64) << 8)
            | (g.Data4[7] as u64),
    )
}
