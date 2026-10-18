//! Where Apple-side paths live on this device. For now the game sees the
//! real folders directly, so paths pass through unchanged.

use std::ffi::CString;

pub unsafe fn host_path_c(path: *const libc::c_char) -> CString {
    if path.is_null() {
        return CString::default();
    }
    std::ffi::CStr::from_ptr(path).to_owned()
}

pub fn apple_executable_path() -> String {
    crate::loader::try_loaded()
        .map(|l| l.path.clone())
        .unwrap_or_default()
}
