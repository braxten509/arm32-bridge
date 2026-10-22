//! The game's standard output and error on the phone, where Android drops
//! them: they go through a pipe, and each line is copied into the log (Unity
//! and Mono write their messages and errors there).

use std::sync::Mutex;

/// The pipe's reading end and the start of a line not finished yet.
static PIPE: Mutex<Option<(i32, Vec<u8>)>> = Mutex::new(None);

/// A FILE for the game's standard output or error: on the phone one end of the
/// log pipe (made on first use), else the process's own file `fd`.
pub fn game_file(fd: i32) -> *mut libc::FILE {
    let target = if cfg!(target_os = "android") { pipe_end().unwrap_or(fd) } else { fd };
    let file = unsafe { libc::fdopen(libc::dup(target), c"w".as_ptr()) };
    unsafe { libc::setvbuf(file, std::ptr::null_mut(), libc::_IOLBF, 0) };
    file
}

/// The writing end of the pipe; the first call makes it and starts the thread
/// that copies lines into the log.
fn pipe_end() -> Option<i32> {
    static WRITE_END: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    *WRITE_END.get_or_init(|| {
        let mut ends = [0i32; 2];
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return None;
        }
        unsafe { libc::fcntl(ends[0], libc::F_SETFL, libc::O_NONBLOCK) };
        *PIPE.lock().unwrap_or_else(|e| e.into_inner()) = Some((ends[0], Vec::new()));
        let reader = ends[0];
        std::thread::Builder::new()
            .name("ios-output".into())
            .spawn(move || loop {
                let mut wait = libc::pollfd { fd: reader, events: libc::POLLIN, revents: 0 };
                unsafe { libc::poll(&mut wait, 1, -1) };
                drain();
            })
            .ok()?;
        Some(ends[1])
    })
}

/// Copies everything waiting in the pipe into the log (the crash report calls
/// this too, so the last lines before a crash are kept).
pub fn drain() {
    // A thread already copying (or a crash inside the copying) is left alone.
    let Ok(mut pipe) = PIPE.try_lock() else { return };
    let Some((reader, pending)) = pipe.as_mut() else { return };
    let mut bytes = [0u8; 4096];
    loop {
        let count = unsafe { libc::read(*reader, bytes.as_mut_ptr() as *mut _, bytes.len()) };
        if count <= 0 {
            break;
        }
        take(pending, &bytes[..count as usize], &mut |line| crate::log!("game: {line}"));
    }
}

/// Adds bytes to the unfinished line and hands on every finished one.
fn take(pending: &mut Vec<u8>, bytes: &[u8], emit: &mut dyn FnMut(&str)) {
    pending.extend_from_slice(bytes);
    while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
        let line: Vec<u8> = pending.drain(..=end).collect();
        emit(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines split across reads come out whole, without their line ends.
    #[test]
    fn lines_come_out_whole() {
        let mut lines = Vec::new();
        let mut pending = Vec::new();
        take(&mut pending, b"Unhandled Exc", &mut |line| lines.push(line.to_string()));
        take(&mut pending, b"eption: X\r\nnext\n", &mut |line| lines.push(line.to_string()));
        assert_eq!(lines, ["Unhandled Exception: X", "next"]);
        assert!(pending.is_empty());
    }
}
