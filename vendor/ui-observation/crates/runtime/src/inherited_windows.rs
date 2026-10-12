//! The only Windows handle/FFI boundary in the runtime.
//!
//! SAFETY invariants:
//! - the numeric value is rejected unless it names an inheritable pipe handle;
//! - `PeekNamedPipe` must fail with access denied, proving the handle cannot read;
//! - inheritance is disabled before `OwnedHandle` assumes sole ownership;
//! - after `OwnedHandle::from_raw_handle`, no other runtime owner closes it.

use std::{fs::File, os::windows::io::FromRawHandle};
use windows_sys::Win32::{
    Foundation::{
        GetHandleInformation, GetLastError, SetHandleInformation, ERROR_ACCESS_DENIED,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE},
    System::Pipes::PeekNamedPipe,
};

pub(super) fn take_write_stream(raw: usize) -> Option<File> {
    if raw == 0 {
        return None;
    }
    let handle = raw as *mut core::ffi::c_void;
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut flags = 0_u32;
    // SAFETY: the numeric handle is only inspected; the return is checked.
    if unsafe { GetHandleInformation(handle, &mut flags) } == 0
        || flags & HANDLE_FLAG_INHERIT == 0
        // SAFETY: GetFileType only inspects the live handle proved above.
        || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE
    {
        return None;
    }
    // A readable pipe succeeds here. A write-only pipe must fail specifically
    // with access denied; broken or otherwise invalid pipes are rejected.
    // SAFETY: null output pointers are explicitly accepted for this probe.
    let peeked = unsafe {
        PeekNamedPipe(
            handle,
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    // SAFETY: GetLastError has no preconditions and is read immediately.
    if peeked != 0 || unsafe { GetLastError() } != ERROR_ACCESS_DENIED {
        return None;
    }
    // SAFETY: handle was proved live; this only disables later inheritance.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return None;
    }
    // SAFETY: validation succeeded and this establishes the runtime's sole owner.
    let owned = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) };
    Some(File::from(owned))
}
