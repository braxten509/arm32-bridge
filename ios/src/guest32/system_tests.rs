//! Darwin layouts and numbers in libSystem's signal, regex, Mach memory and
//! backtrace calls, SQLite with guest callbacks, and dispatch groups and
//! timers. Each test calls the registered handler the way translated game
//! code would.

use super::tests::{alloc, call, cstring, setup};
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, ArgSource, Args, GuestCpu, Ret, Uc};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// A guest copy of `values`.
fn words(values: &[u32]) -> u32 {
    let at = alloc(values.len() as u32 * 4);
    for (i, value) in values.iter().enumerate() {
        mem().write_u32(at + i as u32 * 4, *value);
    }
    at
}

fn three_words(at: u32) -> (u32, u32, u32) {
    (mem().read_u32(at), mem().read_u32(at + 4), mem().read_u32(at + 8))
}

/// Polls `done` every 10 ms for up to 2 s.
fn soon(done: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    done()
}

#[test]
fn signals_use_darwins_numbers_and_layouts() {
    // (signal argument, si_signo of the siginfo) for each handler run.
    static RUNS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    fn handler(_uc: &mut Uc, a: &mut Args) -> Ret {
        let (signal, info) = (a.u32(), a.u32());
        RUNS.lock().unwrap().push((signal, if info == 0 { 0 } else { mem().read_u32(info) }));
        Ret::Void
    }
    setup();
    ios32::register_function("test_darwin_signal_handler", handler);
    let handler = runtime::stub_for("test_darwin_signal_handler");
    const SIGUSR1: u32 = 30; // Linux: 10
    const SA_SIGINFO: u32 = 0x40; // Linux: 4
    // struct sigaction { handler, mask, flags }
    assert_eq!(call("sigaction", [SIGUSR1, words(&[handler, 1 << 1, SA_SIGINFO]), 0, 0], &[]).0, 0);
    let old = alloc(12);
    assert_eq!(call("sigaction", [SIGUSR1, 0, old, 0], &[]).0, 0);
    assert_eq!(three_words(old), (handler, 2, SA_SIGINFO));
    assert_eq!(call("raise", [SIGUSR1, 0, 0, 0], &[]).0, 0);
    assert_eq!(*RUNS.lock().unwrap(), [(30, 30)], "run before raise returns, with Darwin's number");
    // Blocked (SIG_BLOCK 1; bit n-1 = signal n), it waits for SIG_UNBLOCK (2).
    let set = words(&[1 << (SIGUSR1 - 1)]);
    assert_eq!(call("sigprocmask", [1, set, 0, 0], &[]).0, 0);
    assert_eq!(call("raise", [SIGUSR1, 0, 0, 0], &[]).0, 0);
    assert_eq!(RUNS.lock().unwrap().len(), 1, "blocked");
    let was = alloc(4);
    assert_eq!(call("sigprocmask", [2, set, was, 0], &[]).0, 0);
    assert_eq!(mem().read_u32(was), 1 << 29);
    assert_eq!(RUNS.lock().unwrap().len(), 2, "unblocking runs the waiting signal");
    // signal(SIGPIPE 13, SIG_IGN) answers the old handler; an ignored raise does nothing.
    assert_eq!(call("signal", [13, 1, 0, 0], &[]).0, 0);
    assert_eq!(call("raise", [13, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("signal", [13, 0, 0, 0], &[]).0, 1);
    // A crash reporter's SIGSEGV handler is taken (real faults stay the bridge's).
    assert_eq!(call("sigaction", [11, words(&[handler, 0, SA_SIGINFO]), 0, 0], &[]).0, 0);
    assert_eq!(mem().string(call("strsignal", [10, 0, 0, 0], &[]).0), "Bus error", "Darwin's 10 (Linux: SIGUSR1)");
    // sigaltstack: stack_t { sp, size, flags }; none at first (SS_DISABLE 4).
    let (first, second, stack) = (alloc(12), alloc(12), alloc(65536));
    assert_eq!(call("sigaltstack", [words(&[stack, 65536, 0]), first, 0, 0], &[]).0, 0);
    assert_eq!(mem().read_u32(first + 8), 4);
    assert_eq!(call("sigaltstack", [0, second, 0, 0], &[]).0, 0);
    assert_eq!(three_words(second), (stack, 65536, 0));
}

#[test]
fn regex_uses_darwins_flags_and_match_layout() {
    const REG_EXTENDED: u32 = 1;
    const REG_ICASE: u32 = 2;
    const REG_NOSUB: u32 = 4; // glibc: REG_NEWLINE
    const REG_NEWLINE: u32 = 8; // glibc: REG_NOSUB
    // regex_t { re_magic, re_nsub, re_endp, re_g }; regmatch_t { off_t so, eo } (16 bytes).
    let re = alloc(16);
    assert_eq!(call("regcomp", [re, cstring("([a-z]+)-([0-9]+)"), REG_EXTENDED, 0], &[]).0, 0);
    assert_eq!(mem().read_u32(re + 4), 2, "re_nsub");
    let matches = alloc(48);
    assert_eq!(call("regexec", [re, cstring("xx abc-42 yy"), 3, matches], &[0]).0, 0);
    let pairs: Vec<(u64, u64)> = (0..3).map(|i| (mem().read_u64(matches + 16 * i), mem().read_u64(matches + 16 * i + 8))).collect();
    assert_eq!(pairs, [(3, 9), (3, 6), (7, 9)]);
    assert_eq!(call("regexec", [re, cstring("nothing"), 0, 0], &[0]).0, 1, "REG_NOMATCH");
    call("regfree", [re, 0, 0, 0], &[]);
    // Darwin's REG_NEWLINE: ^ matches after a line break.
    assert_eq!(call("regcomp", [re, cstring("^B"), REG_EXTENDED | REG_ICASE | REG_NEWLINE, 0], &[]).0, 0);
    assert_eq!(call("regexec", [re, cstring("a\nb"), 0, 0], &[0]).0, 0);
    call("regfree", [re, 0, 0, 0], &[]);
    // Darwin's REG_NOSUB: regexec leaves the matches alone.
    assert_eq!(call("regcomp", [re, cstring("b"), REG_NOSUB, 0], &[]).0, 0);
    mem().write_bytes(matches, &[0xEE; 16]);
    assert_eq!(call("regexec", [re, cstring("abc"), 1, matches], &[0]).0, 0);
    assert_eq!(mem().bytes(matches, 16), [0xEE; 16]);
    call("regfree", [re, 0, 0, 0], &[]);
    assert_eq!(call("regcomp", [re, cstring("(a"), REG_EXTENDED, 0], &[]).0, 8, "REG_EPAREN");
}

#[test]
fn mach_memory_calls_work_on_guest_memory() {
    const TASK: u32 = 0x103;
    let address = alloc(4);
    assert_eq!(call("vm_allocate", [TASK, address, 8192, 1], &[]).0, 0, "VM_FLAGS_ANYWHERE");
    let block = mem().read_u32(address);
    assert_eq!(block % 4096, 0);
    assert!(mem().bytes(block, 8192).iter().all(|&byte| byte == 0));
    mem().write_bytes(block + 100, b"crash report");
    let (copy, copied) = (alloc(16), alloc(4));
    assert_eq!(call("vm_read_overwrite", [TASK, block + 100, 12, copy], &[copied]).0, 0);
    assert_eq!((mem().bytes(copy, 12), mem().read_u32(copied)), (&b"crash report"[..], 12));
    // Memory that isn't there: KERN_INVALID_ADDRESS (1), not a crash.
    assert_eq!(call("vm_read_overwrite", [TASK, 0x10, 12, copy], &[copied]).0, 1);
    assert_eq!(call("vm_protect", [TASK, block, 8192, 0], &[3]).0, 0);
    assert_eq!(call("vm_deallocate", [TASK, block, 8192, 0], &[]).0, 0);
    // The calling thread's stack: Apple answers its top (the highest address) and size.
    let thread = call("pthread_self", [0; 4], &[]).0;
    let top = call("pthread_get_stackaddr_np", [thread, 0, 0, 0], &[]).0;
    let size = call("pthread_get_stacksize_np", [thread, 0, 0, 0], &[]).0;
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    assert_eq!(top, uc.get_data().stack_top);
    assert!([runtime::MAIN_STACK_SIZE, runtime::THREAD_STACK_SIZE].contains(&size), "{size:#x}");
}

/// Threads the host makes (dispatch workers, timers) that run game code give
/// their guest stack back when they end: a busy serial queue starts a new
/// worker for each burst of work, more over a game's life than the guest
/// has room for stacks.
#[test]
fn host_threads_give_their_guest_stack_back_when_they_end() {
    setup();
    let room = (arm32_bridge::mem::STACK_TOP - arm32_bridge::mem::STACK_BASE) / runtime::THREAD_STACK_SIZE;
    for _ in 0..room + 8 {
        std::thread::spawn(|| {
            let _gil = runtime::hold_gil();
            runtime::engine();
        })
        .join()
        .expect("the thread got a guest stack");
    }
}

#[test]
fn backtrace_follows_apples_frame_chain() {
    // Apple armv7 frames: r7 points at { saved r7, return address }, callers higher up.
    let frames = alloc(16);
    for (i, word) in [frames + 8, 0x3001, 0, 0x2001].iter().enumerate() {
        mem().write_u32(frames + i as u32 * 4, *word);
    }
    let buffer = alloc(16);
    let handler = ios32::function("backtrace").expect("backtrace is registered");
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let (r7, lr) = (uc.reg(7), uc.reg(14));
    uc.set_reg(7, frames);
    uc.set_reg(14, 0x4001);
    let result = handler(uc, &mut Args::new([buffer, 4, 0, 0], 0));
    uc.set_reg(7, r7);
    uc.set_reg(14, lr);
    assert!(matches!(result, Ret::U32(3)));
    assert_eq!([0, 1, 2].map(|i| mem().read_u32(buffer + 4 * i)), [0x4001, 0x3001, 0x2001]);
}

#[test]
fn sqlite_round_trip_with_guest_callbacks() {
    static ROWS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
    static FREED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    // exec's callback(argument, count, values, names): the argument is its answer.
    fn row(_uc: &mut Uc, a: &mut Args) -> Ret {
        let (answer, count, values, names) = (a.u32(), a.u32(), a.u32(), a.u32());
        let text = |list: u32, i: u32| match mem().read_u32(list + 4 * i) {
            0 => "NULL".to_string(),
            pointer => mem().string(pointer),
        };
        ROWS.lock().unwrap().push((0..count).map(|i| format!("{}={}", text(names, i), text(values, i))).collect());
        Ret::U32(answer)
    }
    fn destructor(_uc: &mut Uc, a: &mut Args) -> Ret {
        FREED.lock().unwrap().push(a.u32());
        Ret::Void
    }
    setup();
    ios32::register_function("test_sqlite_row", row);
    ios32::register_function("test_sqlite_destructor", destructor);
    let out = alloc(4);
    assert_eq!(call("sqlite3_open", [cstring(":memory:"), out, 0, 0], &[]).0, 0);
    let db = mem().read_u32(out);
    let error = alloc(4);
    let create = cstring("create table t(a integer, b real, c text); insert into t values(1, 2.5, 'one')");
    assert_eq!(call("sqlite3_exec", [db, create, 0, 0], &[error]).0, 0);
    // The error message is guest text, given back with sqlite3_free.
    assert_eq!(call("sqlite3_exec", [db, cstring("select a from nowhere"), 0, 0], &[error]).0, 1);
    let message = mem().read_u32(error);
    assert!(mem().string(message).contains("nowhere"), "{}", mem().string(message));
    call("sqlite3_free", [message, 0, 0, 0], &[]);

    // Bound values: a double in r2:r3, text whose destructor is game code.
    let (statement, tail) = (alloc(4), alloc(4));
    let sql = cstring("insert into t values(?, ?, ?)");
    assert_eq!(call("sqlite3_prepare_v2", [db, sql, u32::MAX, statement], &[tail]).0, 0);
    assert_eq!(mem().read_u32(tail), sql + 29, "the tail points into the game's text");
    let insert = mem().read_u32(statement);
    let quarter = 0.25f64.to_bits();
    assert_eq!(call("sqlite3_bind_int", [insert, 1, 7, 0], &[]).0, 0);
    assert_eq!(call("sqlite3_bind_double", [insert, 2, quarter as u32, (quarter >> 32) as u32], &[]).0, 0);
    let seven = cstring("seven");
    assert_eq!(call("sqlite3_bind_text", [insert, 3, seven, u32::MAX], &[runtime::stub_for("test_sqlite_destructor")]).0, 0);
    assert_eq!(call("sqlite3_step", [insert, 0, 0, 0], &[]).0, 101, "SQLITE_DONE");
    assert_eq!(call("sqlite3_finalize", [insert, 0, 0, 0], &[]).0, 0);
    assert_eq!(*FREED.lock().unwrap(), [seven], "the game's destructor gets its text back once SQLite is done");

    // exec's game callback sees each row; a non-zero answer stops it (SQLITE_ABORT 4).
    let select = cstring("select a, b, c from t order by a");
    let callback = runtime::stub_for("test_sqlite_row");
    assert_eq!(call("sqlite3_exec", [db, select, callback, 0], &[0]).0, 0);
    assert_eq!(*ROWS.lock().unwrap(), [vec!["a=1", "b=2.5", "c=one"], vec!["a=7", "b=0.25", "c=seven"]]);
    assert_eq!(call("sqlite3_exec", [db, select, callback, 1], &[0]).0, 4);
    assert_eq!(ROWS.lock().unwrap().len(), 3);

    // Columns; column_text is the same guest text until the next step.
    assert_eq!(call("sqlite3_prepare_v2", [db, select, u32::MAX, statement], &[0]).0, 0);
    let query = mem().read_u32(statement);
    assert_eq!(call("sqlite3_step", [query, 0, 0, 0], &[]).0, 100, "SQLITE_ROW");
    assert_eq!(call("sqlite3_column_int", [query, 0, 0, 0], &[]).0, 1);
    let (low, high) = call("sqlite3_column_double", [query, 1, 0, 0], &[]);
    assert_eq!(f64::from_bits(u64::from(low) | u64::from(high) << 32), 2.5);
    let text = call("sqlite3_column_text", [query, 2, 0, 0], &[]).0;
    assert_eq!(mem().string(text), "one");
    assert_eq!(call("sqlite3_column_text", [query, 2, 0, 0], &[]).0, text);
    assert_eq!(call("sqlite3_step", [query, 0, 0, 0], &[]).0, 100);
    assert_eq!(mem().string(call("sqlite3_column_text", [query, 2, 0, 0], &[]).0), "seven");
    assert_eq!(call("sqlite3_step", [query, 0, 0, 0], &[]).0, 101);
    assert_eq!(call("sqlite3_reset", [query, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("sqlite3_finalize", [query, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("sqlite3_close", [db, 0, 0, 0], &[]).0, 0);
}

#[test]
fn dispatch_groups_timers_and_after_run_guest_blocks() {
    use super::objc::bridge::{dispatch_from_host, set_block_owner, Bridge, Special};
    use super::objc::functions::{call_function, Ctx};
    use super::objc::{mirror, testing::Fake};
    setup();
    let g = Fake::new();
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&Default::default()).unwrap())));
    let stack_isa = g.words(&[0; 5]);
    let malloc_isa = g.words(&[0; 5]);
    bridge.lock().unwrap().set_special(stack_isa, Special::StackBlock);
    bridge.lock().unwrap().set_special(malloc_isa, Special::MallocBlock);
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame| dispatch_from_host(bridge, g, frame).unwrap()));
    set_block_owner(g);
    let ctx = Ctx { bridge, g };
    let call = |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&ctx, name, &w).unwrap()
    };
    // void (^)(void) blocks capturing a number; each run notes it.
    let runs: &'static Mutex<Vec<u32>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    g.code(0x5101, move |w| {
        runs.lock().unwrap().push(g.u32_at(w[0] + 20));
        0
    });
    let descriptor = g.words(&[0, 24, g.text("v4@?0")]);
    let block = |n: u32| g.words(&[stack_isa, 1 << 30, 0, 0x5101, descriptor, n]);
    let count = |n: u32| runs.lock().unwrap().iter().filter(|&&r| r == n).count();
    let split = |time: u64| [time as u32, (time >> 32) as u32];

    // A group: blocks sent with dispatch_group_async; wait(DISPATCH_TIME_FOREVER) sees them all done.
    let group = call("dispatch_group_create", &[]) as u32;
    let global = call("dispatch_get_global_queue", &[0, 0]) as u32;
    for n in 1..=3 {
        call("dispatch_group_async", &[group, global, block(n)]);
    }
    assert_eq!(call("dispatch_group_wait", &[group, !0, !0]), 0);
    assert_eq!((count(1), count(2), count(3)), (1, 1, 1));
    // Entered and not left: a wait until now times out (non-zero); notify waits for the leave.
    call("dispatch_group_enter", &[group]);
    assert_ne!(call("dispatch_group_wait", &[group, 0, 0]) as u32, 0);
    let queue = call("dispatch_queue_create", &[g.text("timers"), 0]) as u32;
    call("dispatch_group_notify", &[group, queue, block(4)]);
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(count(4), 0, "the group isn't empty yet");
    call("dispatch_group_leave", &[group]);
    assert!(soon(|| count(4) == 1), "notify ran once the group emptied");

    // dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 20 ms), queue, block)
    let later = call("dispatch_time", &[0, 0, 20_000_000, 0]);
    let [low, high] = split(later);
    call("dispatch_after", &[low, high, queue, block(5)]);
    assert!(soon(|| count(5) == 1), "dispatch_after ran its block");

    // A timer source: starts suspended, fires every 5 ms once resumed.
    let kind = ios32::data_address("_dispatch_source_type_timer").expect("_dispatch_source_type_timer");
    let source = call("dispatch_source_create", &[kind, 0, 0, queue]) as u32;
    assert_ne!(source, 0);
    call("dispatch_source_set_event_handler", &[source, block(6)]);
    let [start_low, start_high] = split(call("dispatch_time", &[0, 0, 0, 0]));
    call("dispatch_source_set_timer", &[source, start_low, start_high, 5_000_000, 0, 0, 0]);
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(count(6), 0, "a new source is suspended");
    call("dispatch_resume", &[source]);
    assert!(soon(|| count(6) >= 2), "the timer fires again and again");
    call("dispatch_suspend", &[source]);
    std::thread::sleep(std::time::Duration::from_millis(30));
    let fired = count(6);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(count(6), fired, "suspended, it stops");
}

#[test]
fn a_signal_raised_in_its_own_handler_runs_when_the_handler_returns() {
    // How many times the handler ran, and how many while the first run was still going.
    static RUNS: AtomicU32 = AtomicU32::new(0);
    static DURING: AtomicU32 = AtomicU32::new(0);
    fn handler(uc: &mut Uc, a: &mut Args) -> Ret {
        let signal = a.u32();
        if RUNS.fetch_add(1, Ordering::SeqCst) == 0 {
            // Blocked while its handler runs (no SA_NODEFER): held until it returns.
            let raise = ios32::function("raise").unwrap();
            raise(uc, &mut Args::new([signal, 0, 0, 0], 0));
            DURING.store(RUNS.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        Ret::Void
    }
    setup();
    ios32::register_function("test_reraising_signal_handler", handler);
    const SIGUSR2: u32 = 31;
    let action = words(&[runtime::stub_for("test_reraising_signal_handler"), 0, 0]);
    assert_eq!(call("sigaction", [SIGUSR2, action, 0, 0], &[]).0, 0);
    assert_eq!(call("raise", [SIGUSR2, 0, 0, 0], &[]).0, 0);
    assert_eq!(DURING.load(Ordering::SeqCst), 1, "not run inside its own handler");
    assert_eq!(RUNS.load(Ordering::SeqCst), 2, "run as soon as the first handler returned");
}

#[test]
fn vm_allocate_at_a_taken_address_answers_no_space() {
    const TASK: u32 = 0x103;
    let address = alloc(4);
    assert_eq!(call("vm_allocate", [TASK, address, 8192, 1], &[]).0, 0);
    let block = mem().read_u32(address);
    mem().write_bytes(block + 16, b"still here");
    // VM_FLAGS_FIXED (0) over memory already handed out: KERN_NO_SPACE (3), nothing replaced.
    mem().write_u32(address, block);
    assert_eq!(call("vm_allocate", [TASK, address, 4096, 0], &[]).0, 3);
    assert_eq!(mem().bytes(block + 16, 10), b"still here");
    assert_eq!(mem().read_u32(address), block, "the address is left as it was");
    // Over the program's own heap (malloc's blocks) too.
    let heap_block = alloc(4096);
    mem().write_u32(heap_block, 0x1234_5678);
    mem().write_u32(address, heap_block & !0xFFF);
    assert_eq!(call("vm_allocate", [TASK, address, 4096, 0], &[]).0, 3);
    assert_eq!(mem().read_u32(heap_block), 0x1234_5678);
    call("vm_deallocate", [TASK, block, 8192, 0], &[]);
}

#[test]
fn sqlite_bind_text_destructors_follow_sqlites_contract() {
    static FREED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    fn destructor(_uc: &mut Uc, a: &mut Args) -> Ret {
        FREED.lock().unwrap().push(a.u32());
        Ret::Void
    }
    let freed = || FREED.lock().unwrap().clone();
    setup();
    ios32::register_function("test_sqlite_contract_destructor", destructor);
    let game_destructor = runtime::stub_for("test_sqlite_contract_destructor");
    let out = alloc(4);
    assert_eq!(call("sqlite3_open", [cstring(":memory:"), out, 0, 0], &[]).0, 0);
    let db = mem().read_u32(out);
    assert_eq!(call("sqlite3_exec", [db, cstring("create table t(x text)"), 0, 0], &[0]).0, 0);
    assert_eq!(call("sqlite3_prepare_v2", [db, cstring("insert into t values(?)"), u32::MAX, out], &[0]).0, 0);
    let insert = mem().read_u32(out);
    // Length -1 (up to the end mark): SQLite uses the game's text until it is done with it.
    let (first, second, third) = (cstring("first"), cstring("second"), cstring("third"));
    assert_eq!(call("sqlite3_bind_text", [insert, 1, first, u32::MAX], &[game_destructor]).0, 0);
    assert_eq!(freed(), [], "not while SQLite still uses it");
    // Binding the parameter again: SQLite is done with the first text.
    assert_eq!(call("sqlite3_bind_text", [insert, 1, second, u32::MAX], &[u32::MAX]).0, 0, "SQLITE_TRANSIENT");
    assert_eq!(freed(), [first]);
    // A failed bind (SQLITE_RANGE 25) still disposes of the text; a NULL text never reaches the destructor.
    assert_eq!(call("sqlite3_bind_text", [insert, 9, third, 5], &[game_destructor]).0, 25);
    assert_eq!(freed(), [first, third]);
    assert_eq!(call("sqlite3_bind_text", [insert, 1, 0, u32::MAX], &[game_destructor]).0, 0);
    assert_eq!(freed(), [first, third]);
    // SQLITE_STATIC: SQLite never disposes of it.
    let fourth = cstring("fourth");
    assert_eq!(call("sqlite3_bind_text", [insert, 1, fourth, 6], &[0]).0, 0);
    assert_eq!(call("sqlite3_step", [insert, 0, 0, 0], &[]).0, 101);
    let last = cstring("last");
    assert_eq!(call("sqlite3_reset", [insert, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("sqlite3_bind_text", [insert, 1, last, u32::MAX], &[game_destructor]).0, 0);
    assert_eq!(call("sqlite3_step", [insert, 0, 0, 0], &[]).0, 101);
    assert_eq!(freed(), [first, third]);
    assert_eq!(call("sqlite3_finalize", [insert, 0, 0, 0], &[]).0, 0);
    assert_eq!(freed(), [first, third, last], "finalizing is when SQLite is done with the last one");
    assert_eq!(call("sqlite3_close", [db, 0, 0, 0], &[]).0, 0);
}

/// GIL mode: a game callback inside sqlite3_exec may let go of the guest
/// lock; another thread then calling into the same connection must not wait
/// for SQLite's lock while holding the guest lock (the callback could never
/// get it back).
#[test]
#[ignore = "GIL mode: test-pc.sh runs it with ARM32_PARALLEL=0"]
fn gil_mode_sqlite_callbacks_let_other_threads_wait_for_the_connection() {
    static DB: AtomicU32 = AtomicU32::new(0);
    static OTHER: Mutex<Option<std::sync::mpsc::Receiver<u32>>> = Mutex::new(None);
    // The row callback: starts a thread that prepares on the same connection,
    // and lets go of the guest lock until that thread is in SQLite.
    fn row(_uc: &mut Uc, _a: &mut Args) -> Ret {
        let (entering, entered) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        *OTHER.lock().unwrap() = Some(result);
        std::thread::spawn(move || {
            let _gil = runtime::hold_gil();
            let out = alloc(4);
            let sql = cstring("select 2");
            entering.send(()).unwrap();
            let answer = call("sqlite3_prepare_v2", [DB.load(Ordering::SeqCst), sql, u32::MAX, out], &[0]).0;
            finished.send(answer).unwrap();
        });
        runtime::without_gil(|| {
            entered.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        });
        Ret::U32(0)
    }
    setup();
    assert!(!runtime::parallel(), "run with ARM32_PARALLEL=0 (test-pc.sh does)");
    ios32::register_function("test_sqlite_row_letting_go", row);
    let out = alloc(4);
    assert_eq!(call("sqlite3_open", [cstring(":memory:"), out, 0, 0], &[]).0, 0);
    DB.store(mem().read_u32(out), Ordering::SeqCst);
    let (sql, callback) = (cstring("select 1"), runtime::stub_for("test_sqlite_row_letting_go"));
    let (done, exec) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let answer = call("sqlite3_exec", [DB.load(Ordering::SeqCst), sql, callback, 0], &[0]).0;
        done.send(answer).unwrap();
    });
    let wait = std::time::Duration::from_millis(500);
    assert_eq!(exec.recv_timeout(wait), Ok(0), "deadlock: the callback couldn't get the guest lock back");
    let other = OTHER.lock().unwrap().take().expect("the callback ran");
    assert_eq!(other.recv_timeout(wait), Ok(0), "the other thread's prepare went through after exec");
}
