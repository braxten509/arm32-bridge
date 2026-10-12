//! Log lines go to stderr and, on Android, to logcat (tag "arm32bridge").

pub fn line(text: &str) {
    write(4, "arm32bridge", text.as_bytes());
}

/// `priority` uses Android's numbers (2 verbose … 7 fatal).
pub fn write(priority: u32, tag: &str, message: &[u8]) {
    let text = String::from_utf8_lossy(message);
    let text = text.trim_end();
    #[cfg(target_os = "android")]
    {
        let tag = std::ffi::CString::new(tag).unwrap_or_default();
        let body = std::ffi::CString::new(text.replace('\0', "")).unwrap_or_default();
        unsafe { __android_log_write(priority as i32, tag.as_ptr(), body.as_ptr()) };
    }
    let letter = ["?", "?", "V", "D", "I", "W", "E", "F"].get(priority as usize).copied().unwrap_or("?");
    eprintln!("[{letter}/{tag}] {text}");
}

#[cfg(target_os = "android")]
#[link(name = "log")]
extern "C" {
    fn __android_log_write(priority: i32, tag: *const libc::c_char, text: *const libc::c_char) -> i32;
}
