// Vtable field names are mandated by the COM ABI / MIDL convention.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;
use winapi::shared::guiddef::GUID;
use winapi::shared::ntdef::HANDLE;
use winapi::shared::winerror::{E_NOINTERFACE, E_POINTER, S_OK};

use super::super::com_interfaces::{
    IID_IDefaultTerminalMarker, IID_ITerminalHandoff, IID_ITerminalHandoff2, IID_ITerminalHandoff3,
    IID_IUNKNOWN,
};
use super::super::types::TerminalStartupInfo;
use super::establish::establish_pty_handoff;

#[repr(C)]
struct Instance {
    vtable: *const Vtable,
    refcount: AtomicU32,
}

unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

#[repr(C)]
pub(super) struct Vtable {
    pub(super) QueryInterface: unsafe extern "system" fn(
        This: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> i32,
    pub(super) AddRef: unsafe extern "system" fn(This: *mut c_void) -> u32,
    pub(super) Release: unsafe extern "system" fn(This: *mut c_void) -> u32,
    pub(super) EstablishPtyHandoff: unsafe extern "system" fn(
        This: *mut c_void,
        in_handle: *mut HANDLE,
        out_handle: *mut HANDLE,
        signal: HANDLE,
        reference: HANDLE,
        server: HANDLE,
        client: HANDLE,
        startup_info: *const TerminalStartupInfo,
    ) -> i32,
}

unsafe fn instance_from_this(this: *mut c_void) -> *mut Instance {
    this as *mut Instance
}

pub(super) unsafe extern "system" fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> i32 {
    if iid.is_null() || interface.is_null() {
        return E_POINTER;
    }
    let requested = &*iid;
    let is_handoff3 = guid_eq(requested, &IID_ITerminalHandoff3);
    let is_handoff2 = guid_eq(requested, &IID_ITerminalHandoff2);
    let is_handoff1 = guid_eq(requested, &IID_ITerminalHandoff);
    let is_marker = guid_eq(requested, &IID_IDefaultTerminalMarker);
    let is_unknown = guid_eq(requested, &IID_IUNKNOWN);

    if is_handoff3 || is_marker || is_unknown {
        *interface = this;
        add_ref(this);
        S_OK
    } else if is_handoff1 || is_handoff2 {
        // Inbox conhost only QIs for v3.
        *interface = std::ptr::null_mut();
        E_NOINTERFACE
    } else {
        *interface = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let inst = instance_from_this(this);
    (*inst).refcount.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let inst = instance_from_this(this);
    let prev = (*inst).refcount.fetch_sub(1, Ordering::SeqCst);
    // Singleton lives for process lifetime; revoked via HandoffGuard::drop.
    prev.saturating_sub(1)
}

pub(super) static VTABLE: Vtable = Vtable {
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
    EstablishPtyHandoff: establish_pty_handoff,
};

static SINGLETON: Mutex<Option<Box<Instance>>> = Mutex::new(None);

pub(crate) fn take_singleton() -> *mut c_void {
    let mut guard = SINGLETON.lock();
    if guard.is_none() {
        let inst = Box::new(Instance {
            vtable: &VTABLE,
            refcount: AtomicU32::new(1),
        });
        *guard = Some(inst);
    }
    guard.as_mut().unwrap().as_mut() as *mut Instance as *mut c_void
}

pub(super) fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.Data1 == b.Data1 && a.Data2 == b.Data2 && a.Data3 == b.Data3 && a.Data4 == b.Data4
}
