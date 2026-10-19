//! Log lines and the end-of-run summary.

use std::io::Write;
use std::sync::Mutex;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::report::line(&format!($($arg)*))
    };
}

static OUT: Mutex<()> = Mutex::new(());
static MILESTONE: Mutex<Option<(bool, String)>> = Mutex::new(None);
static RUN_LOOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Seconds since the layer started.
pub fn seconds_since_start() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

pub fn reached_run_loop() {
    RUN_LOOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Where log lines also go (IOS_LOG_FILE), for the phone app, whose
/// standard error output is not kept.
fn log_file() -> Option<&'static Mutex<std::fs::File>> {
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let path = std::env::var_os("IOS_LOG_FILE")?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(Mutex::new)
    })
    .as_ref()
}

pub fn line(text: &str) {
    let _guard = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let tid = unsafe { libc::gettid() };
    let _ = writeln!(std::io::stderr(), "[ios {tid}] {text}");
    if let Some(file) = log_file() {
        if let Ok(mut file) = file.try_lock() {
            let _ = writeln!(file, "[ios {tid}] {text}");
        }
    }
}

pub fn reached_did_finish(in_game: bool, place: &str) {
    *MILESTONE.lock().unwrap() = Some((in_game, place.to_string()));
}

/// The phone app's end of the "game stopped" pipe (see notice_pipe), or -1.
static NOTICE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// A pipe the phone app's screen watches: when the game stops by mistake,
/// a byte arrives (1: a report was saved, 2: it couldn't be) and the screen
/// shows a message over the game; the process ends when the player closes
/// it. Gives the end to watch (-1 on failure).
pub fn notice_pipe() -> i32 {
    let mut fds = [-1i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return -1;
    }
    NOTICE.store(fds[1], std::sync::atomic::Ordering::SeqCst);
    fds[0]
}

/// A thread that stops after another one already did waits here when the
/// phone app shows a message: the first one ends the process or puts the
/// message up, and the process must not end under it. The phone's main
/// thread draws the message, so it never waits.
fn wait_for_first_stop() {
    let main_thread = unsafe { libc::gettid() == libc::getpid() };
    if NOTICE.load(std::sync::atomic::Ordering::SeqCst) >= 0 && !main_thread {
        loop {
            unsafe { libc::pause() };
        }
    }
}

/// Copies the run log to IOS_REPORT_DIR as "<IOS_GAME_NAME> <date> <time>.txt",
/// where the player can read it without root. True when it was saved.
fn save_report() -> bool {
    let (Some(folder), Some(log)) = (std::env::var_os("IOS_REPORT_DIR"), std::env::var_os("IOS_LOG_FILE")) else { return false };
    let name = std::env::var("IOS_GAME_NAME").unwrap_or_else(|_| "game".into()).replace('/', "-");
    let mut stamp = [0u8; 32];
    let written = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut parts);
        libc::strftime(stamp.as_mut_ptr() as *mut libc::c_char, stamp.len(), c"%Y-%m-%d %H%M%S".as_ptr(), &parts)
    };
    let stamp = String::from_utf8_lossy(&stamp[..written]);
    let target = std::path::Path::new(&folder).join(format!("{name} {stamp}.txt"));
    let _ = std::fs::create_dir_all(&folder);
    match std::fs::copy(&log, &target) {
        Ok(_) => {
            line(&format!("report saved: {}", target.display()));
            true
        }
        Err(error) => {
            line(&format!("report not saved to {}: {error}", target.display()));
            false
        }
    }
}

/// Prints the summary and ends the process. When the game stopped by
/// mistake (code not 0) on the phone, the report is saved and the game
/// screen shows a message; the process ends when the player closes it.
pub fn finish(code: i32) -> ! {
    static ONCE: Mutex<bool> = Mutex::new(false);
    {
        let mut done = ONCE.lock().unwrap_or_else(|e| e.into_inner());
        if *done {
            wait_for_first_stop();
            unsafe { libc::_exit(code) };
        }
        *done = true;
    }
    let mut summary = String::new();
    if let Some(report) = crate::loader::LOAD_REPORT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        summary += &format!(
            "imports: {} functions and {} data found; {} functions, {} data, {} classes missing; {} weak imports left empty; {} name constants made\n",
            report.functions_found,
            report.data_found,
            report.functions_missing.len(),
            report.data_missing.len(),
            report.classes_missing.len(),
            report.weak_left_empty.len(),
            report.data_named.len()
        );
        if std::env::var_os("IOS_LIST_MISSING").is_some() {
            summary += &format!("missing functions: {}\n", report.functions_missing.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("missing data: {}\n", report.data_missing.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("missing classes: {}\n", report.classes_missing.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("weak imports left empty: {}\n", report.weak_left_empty.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("name constants made: {}\n", report.data_named.iter().cloned().collect::<Vec<_>>().join(" "));
        }
    }
    let hits = crate::trap::hits();
    if !hits.is_empty() {
        summary += "missing functions reached (calls):";
        for (name, count) in &hits {
            summary += &format!(" {name}({count})");
        }
        summary += "\n";
    }
    let methods = crate::objc::runtime::missing_methods();
    if !methods.is_empty() {
        summary += "missing methods reached (calls):";
        for (name, count) in &methods {
            summary += &format!(" {name}({count})");
        }
        summary += "\n";
    }
    match MILESTONE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some((true, place)) => summary += &format!("MILESTONE: the game's own didFinishLaunching code ran ({place})\n"),
        Some((false, place)) => summary += &format!("milestone not met: didFinishLaunching is not game code ({place})\n"),
        None => summary += "milestone not met: didFinishLaunching was not reached\n",
    }
    if RUN_LOOP.load(std::sync::atomic::Ordering::Relaxed) {
        summary += &format!("MILESTONE: start-up finished and the run loop ran ({} timer calls)\n", crate::fw::runloop::timers_fired());
    }
    let frames = crate::fw::eagl::frames_shown();
    if frames > 0 {
        summary += &format!("MILESTONE: the game showed {frames} frames\n");
    }
    for text in summary.lines() {
        line(text);
    }
    let _ = std::io::stderr().flush();
    let notice = NOTICE.load(std::sync::atomic::Ordering::SeqCst);
    // The phone's main thread draws the message, so it can't wait for itself.
    let main_thread = unsafe { libc::gettid() == libc::getpid() };
    if code != 0 && notice >= 0 && !main_thread {
        let saved = save_report();
        unsafe {
            libc::alarm(0);
            let byte = if saved { 1u8 } else { 2u8 };
            if libc::write(notice, [byte].as_ptr() as *const libc::c_void, 1) == 1 {
                loop {
                    libc::pause();
                }
            }
        }
    } else if code != 0 {
        save_report();
    }
    unsafe { libc::_exit(code) };
}

static CRASHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PROBE_PIPE: [std::sync::atomic::AtomicI32; 2] = [std::sync::atomic::AtomicI32::new(-1), std::sync::atomic::AtomicI32::new(-1)];

/// Reads a frame record (saved x29, x30) without faulting on a bad address:
/// the kernel copies it through a pipe and reports a bad address as an error.
fn read_frame(frame: usize) -> Option<[usize; 2]> {
    use std::sync::atomic::Ordering;
    let mut pair = [0usize; 2];
    unsafe {
        let local = libc::iovec { iov_base: pair.as_mut_ptr() as *mut _, iov_len: 16 };
        let remote = libc::iovec { iov_base: frame as *mut _, iov_len: 16 };
        if libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) == 16 {
            return Some(pair);
        }
        let (read_end, write_end) = (PROBE_PIPE[0].load(Ordering::Relaxed), PROBE_PIPE[1].load(Ordering::Relaxed));
        if read_end < 0 || libc::write(write_end, frame as *const _, 16) != 16 {
            return None;
        }
        (libc::read(read_end, pair.as_mut_ptr() as *mut _, 16) == 16).then_some(pair)
    }
}

/// Reads `out.len()` bytes at an address that may be bad (None if it can't be read).
fn read_memory(address: usize, out: &mut [u8]) -> bool {
    unsafe {
        let local = libc::iovec { iov_base: out.as_mut_ptr() as *mut _, iov_len: out.len() };
        let remote = libc::iovec { iov_base: address as *mut _, iov_len: out.len() };
        libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) == out.len() as isize
    }
}

/// For each register that points at readable memory: which mapping it is in
/// and the 64 bytes around it (16 before, 48 after), to see what overwrote
/// an object.
fn describe_pointed_memory(registers: &[u64]) {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let mapping = |address: usize| -> String {
        for row in maps.lines() {
            let mut fields = row.split_whitespace();
            let Some((start, end)) = fields.next().and_then(|range| range.split_once('-')) else { continue };
            let (Ok(start), Ok(end)) = (usize::from_str_radix(start, 16), usize::from_str_radix(end, 16)) else { continue };
            if address >= start && address < end {
                let rest: Vec<&str> = fields.collect();
                let name = rest.get(4).copied().unwrap_or("(anonymous)");
                return format!("{} +{:#x} in {start:#x}..{end:#x}", name, address - start);
            }
        }
        "not mapped".into()
    };
    let mut seen = Vec::new();
    for (index, &value) in registers.iter().enumerate() {
        // Top byte ignored, as the processor does for addresses.
        let address = (value as usize) & 0x00ff_ffff_ffff_ffff;
        if address < 0x10000 || seen.contains(&address) {
            continue;
        }
        let mut bytes = [0u8; 64];
        if !read_memory(address.wrapping_sub(16), &mut bytes) {
            continue;
        }
        seen.push(address);
        let words: Vec<String> = bytes.chunks(8).map(|w| format!("{:016x}", u64::from_le_bytes(w.try_into().unwrap()))).collect();
        line(&format!("  x{index} -> {}", mapping(address)));
        line(&format!("     -16: {} {} | {}", words[0], words[1], words[2..].join(" ")));
    }
}

/// Prints where a crash happened (place, registers, call chain) and stops.
/// The report takes locks and allocates, which a crash may have left busy:
/// a second crash ends at once, and a timer ends a report that hangs.
extern "C" fn on_crash(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    if CRASHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        wait_for_first_stop();
        unsafe { libc::_exit(1) };
    }
    unsafe {
        libc::alarm(10);
        let context = &*(context as *const libc::ucontext_t);
        let m = &context.uc_mcontext;
        let fault = (*info).si_addr() as usize;
        line(&format!("CRASH: signal {signal} at {} reading/writing {fault:#x}", crate::loader::describe_address(m.pc as usize)));
        line(&format!("  called from {}", crate::loader::describe_address(m.regs[30] as usize)));
        let mut text = String::new();
        for (index, value) in m.regs.iter().enumerate() {
            text += &format!("x{index}={value:#x} ");
            if index % 6 == 5 {
                line(&format!("  {text}"));
                text.clear();
            }
        }
        line(&format!("  {text}sp={:#x}", m.sp));
        describe_pointed_memory(&m.regs[..29]);
        let mut frame = m.regs[29] as usize;
        for depth in 0..24 {
            if frame == 0 || frame % 8 != 0 {
                break;
            }
            let Some(pair) = read_frame(frame) else {
                line("  (call chain stops: frame not readable)");
                break;
            };
            if pair[1] == 0 {
                break;
            }
            line(&format!("  #{depth} {}", crate::loader::describe_address(pair[1])));
            if pair[0] <= frame {
                break;
            }
            frame = pair[0];
        }
    }
    finish(1);
}

/// The call chain of the calling thread, from its frame pointer (game code included).
#[inline(never)]
pub fn call_chain() -> String {
    let mut frame: usize;
    unsafe { std::arch::asm!("mov {}, x29", out(reg) frame, options(nomem, nostack)) };
    let mut text = String::new();
    for _ in 0..16 {
        if frame == 0 || frame % 8 != 0 {
            break;
        }
        let Some(pair) = read_frame(frame) else { break };
        if pair[1] == 0 {
            break;
        }
        text += &format!(" <- {}", crate::loader::describe_address(pair[1]));
        if pair[0] <= frame {
            break;
        }
        frame = pair[0];
    }
    text
}

/// IOS_CRASH_AT=<seconds> (test switch): at that time a thread reads a bad
/// address, to check what the player sees when a game stops by mistake.
pub fn schedule_test_crash() {
    let Some(seconds) = std::env::var("IOS_CRASH_AT").ok().and_then(|v| v.parse::<f64>().ok()) else { return };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        line(&format!("test crash at {seconds} s (IOS_CRASH_AT)"));
        unsafe { std::ptr::read_volatile(8 as *const usize) };
    });
}

/// IOS_STACKS_AT=<seconds>: at that time every thread writes its call chain
/// to the log (to see where a game is waiting or busy).
pub fn schedule_stack_dump() {
    let Some(seconds) = std::env::var("IOS_STACKS_AT").ok().and_then(|v| v.parse::<f64>().ok()) else { return };
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_stack_request as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigaction(STACK_SIGNAL, &action, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        line(&format!("call chains of every thread at {seconds} s:"));
        let me = unsafe { libc::gettid() };
        if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
            for task in tasks.flatten() {
                let Some(tid) = task.file_name().to_str().and_then(|t| t.parse::<i32>().ok()) else { continue };
                if tid != me {
                    unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, STACK_SIGNAL) };
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    });
}

const STACK_SIGNAL: i32 = 42;

extern "C" fn on_stack_request(_signal: i32, _info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    unsafe {
        let m = &(*(context as *const libc::ucontext_t)).uc_mcontext;
        let mut text = format!("thread {}: at {}", libc::gettid(), crate::loader::describe_address(m.pc as usize));
        text += &format!(" <- {}", crate::loader::describe_address(m.regs[30] as usize));
        let mut frame = m.regs[29] as usize;
        for _ in 0..12 {
            if frame == 0 || frame % 8 != 0 {
                break;
            }
            let Some(pair) = read_frame(frame) else { break };
            if pair[1] == 0 {
                break;
            }
            text += &format!(" <- {}", crate::loader::describe_address(pair[1]));
            if pair[0] <= frame {
                break;
            }
            frame = pair[0];
        }
        line(&text);
    }
}

pub fn install_crash_handler() {
    unsafe {
        let mut fds = [-1i32; 2];
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) == 0 {
            PROBE_PIPE[0].store(fds[0], std::sync::atomic::Ordering::Relaxed);
            PROBE_PIPE[1].store(fds[1], std::sync::atomic::Ordering::Relaxed);
        }
        let stack_size = 256 * 1024;
        let stack = libc::mmap(std::ptr::null_mut(), stack_size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        let alt = libc::stack_t { ss_sp: stack, ss_flags: 0, ss_size: stack_size };
        libc::sigaltstack(&alt, std::ptr::null_mut());
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_crash as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGTRAP, libc::SIGABRT] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}
