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

pub fn reached_run_loop() {
    RUN_LOOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn line(text: &str) {
    let _guard = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let tid = unsafe { libc::gettid() };
    let _ = writeln!(std::io::stderr(), "[ios {tid}] {text}");
}

pub fn reached_did_finish(in_game: bool, place: &str) {
    *MILESTONE.lock().unwrap() = Some((in_game, place.to_string()));
}

/// Prints the summary and ends the process.
pub fn finish(code: i32) -> ! {
    static ONCE: Mutex<bool> = Mutex::new(false);
    {
        let mut done = ONCE.lock().unwrap_or_else(|e| e.into_inner());
        if *done {
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
    for text in summary.lines() {
        line(text);
    }
    let _ = std::io::stderr().flush();
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

/// Prints where a crash happened (place, registers, call chain) and stops.
/// The report takes locks and allocates, which a crash may have left busy:
/// a second crash ends at once, and a timer ends a report that hangs.
extern "C" fn on_crash(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    if CRASHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
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
        for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGTRAP] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}
