//! Behavior that could silently break: Darwin layouts, Apple's 64-bit
//! argument rule, errno numbers, static pthread initializers. Each test
//! calls the registered handler the way translated game code would.

use super::libsystem::{RUNE_ALPHA, RUNE_LOWER, RUNE_LOWER_MAP, RUNE_TYPES, RUNE_XDIGIT};
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, ArgSource, Args, GuestCpu};

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = runtime::set_platform(&ios32::APPLE32);
        super::register_all();
        runtime::init(false);
    });
}

fn alloc(size: u32) -> u32 {
    setup();
    super::alloc(size)
}

fn cstring(text: &str) -> u32 {
    setup();
    super::cstring(text)
}

/// Calls registered function `name` with r0-r3 and stack words; returns (r0, r1).
fn call(name: &str, registers: [u32; 4], stack: &[u32]) -> (u32, u32) {
    setup();
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let sp = alloc(64 + stack.len() as u32 * 4);
    for (i, word) in stack.iter().enumerate() {
        mem().write_u32(sp + i as u32 * 4, *word);
    }
    let handler = ios32::function(name).unwrap_or_else(|| panic!("{name} is not registered"));
    uc.get_data_mut().current_call = Box::leak(name.to_string().into_boxed_str());
    let result = handler(uc, &mut Args::new(registers, sp));
    runtime::write_result(uc, result);
    (uc.reg(0), uc.reg(1))
}

fn errno() -> u32 {
    let (slot, _) = call("__error", [0; 4], &[]);
    mem().read_u32(slot)
}

#[test]
fn doubles_take_the_next_two_words() {
    // __sincos_stret(struct*, double): the double is in r1:r2, not r2:r3.
    let out = alloc(16);
    let x = 0.5f64.to_bits();
    call("__sincos_stret", [out, x as u32, (x >> 32) as u32, 0xDEAD], &[]);
    assert_eq!(f64::from_bits(mem().read_u64(out)), 0.5f64.sin());
    assert_eq!(f64::from_bits(mem().read_u64(out + 8)), 0.5f64.cos());
    // snprintf(buffer, size, format, double): split between r3 and the first stack word.
    let buffer = alloc(32);
    let format = cstring("%.2f|%d");
    let y = 2.25f64.to_bits();
    call("snprintf", [buffer, 32, format, y as u32], &[(y >> 32) as u32, 7]);
    assert_eq!(mem().string(buffer), "2.25|7");
    // A host call to guest code lays out 64-bit arguments the same way.
    use arm32_bridge::runtime::{lay_out_arguments, Arg};
    let pair = 0x1111_2222_3333_4444;
    assert_eq!(lay_out_arguments(&[Arg::Word(1), Arg::Pair(pair)], true), ([1, 0x3333_4444, 0x1111_2222, 0], vec![]));
    assert_eq!(lay_out_arguments(&[Arg::Word(1), Arg::Word(2), Arg::Word(3), Arg::Pair(pair)], true).1, vec![0x1111_2222]);
    assert_eq!(lay_out_arguments(&[Arg::Word(1), Arg::Pair(pair)], false).0, [1, 0, 0x3333_4444, 0x1111_2222]);
}

#[test]
fn stat_fills_darwins_layout() {
    let path = std::env::temp_dir().join(format!("guest32-stat-{}", std::process::id()));
    std::fs::write(&path, b"12345").unwrap();
    let out = alloc(super::libsystem::STAT_SIZE);
    let (result, _) = call("stat", [cstring(path.to_str().unwrap()), out, 0, 0], &[]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(result, 0);
    assert_eq!(mem().read_u16(out + 4) as u32 & 0o170000, 0o100000, "st_mode at 4 says a regular file");
    assert_eq!(mem().read_u64(out + 60), 5, "st_size at 60");
    assert_eq!(mem().read_u32(out + 76), 4096, "st_blksize at 76");
}

#[test]
fn ctype_tables_sit_where_inline_code_reads_them() {
    setup();
    let locale = super::data_address("_DefaultRuneLocale").expect("_DefaultRuneLocale is registered");
    let a = mem().read_u32(locale + RUNE_TYPES + 4 * b'a' as u32);
    assert_eq!(a & (RUNE_ALPHA | RUNE_LOWER | RUNE_XDIGIT), RUNE_ALPHA | RUNE_LOWER | RUNE_XDIGIT);
    assert_eq!(a & 0xff, 10, "a hex digit's value in the low byte");
    assert_eq!(mem().read_u32(locale + RUNE_LOWER_MAP + 4 * b'Z' as u32), b'z' as u32);
    assert_eq!(call("__maskrune", [0x263A, RUNE_ALPHA, 0, 0], &[]).0, 0);
}

#[test]
fn errno_uses_darwins_numbers() {
    let base = std::env::temp_dir().join(format!("guest32-errno-{}", std::process::id()));
    let (from, to) = (base.join("from"), base.join("to"));
    std::fs::create_dir_all(&from).unwrap();
    std::fs::create_dir_all(to.join("inside")).unwrap();
    let (result, _) = call("rename", [cstring(from.to_str().unwrap()), cstring(to.to_str().unwrap()), 0, 0], &[]);
    let error = errno();
    std::fs::remove_dir_all(&base).unwrap();
    assert_eq!(result as i32, -1);
    assert_eq!(error, 66, "ENOTEMPTY is 66 on Darwin (39 on Linux)");
}

#[test]
fn unknown_data_is_zeroed_memory_that_names_itself_when_called() {
    // Only reached through pointers, so taken for data; but it may be a
    // function whose address is taken (a GL entry point in a table).
    setup();
    let address = ios32::unknown_data("glMadeUpEntryPoint");
    assert!(mem().bytes(address, 64).iter().all(|&byte| byte == 0));
    assert!(arm32_bridge::mem::in_stub_area(address), "a call to it reaches a stand-in");
    let slot = (address - arm32_bridge::mem::stub_base()) / 8;
    assert_eq!(runtime::stub(slot).map(|stub| stub.name), Some("glMadeUpEntryPoint"));
    assert_eq!(ios32::unknown_data("glMadeUpEntryPoint"), address, "one block per name");
}

/// The C++ runtime loaded once, as `run` loads it (call before holding the GIL).
fn cxx() {
    setup();
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _gil = runtime::hold_gil();
        super::open_cxx_library(runtime::engine()).expect("the C++ runtime loads");
    });
}

fn guest_export(name: &str) -> u32 {
    *runtime::shared().guest_exports.get(name).unwrap_or_else(|| panic!("no guest export {name}"))
}

#[test]
fn cxx_runtime_imports_are_all_provided() {
    cxx();
    assert_eq!(ios32::missing_library_imports(), Vec::<String>::new());
}

#[test]
fn cxx_runtime_checks_pass_on_this_c_library() {
    // guest32-cxx/test/cxxtest.cpp (exceptions through the SjLj unwinder,
    // streams, locale, guards, threads' keys), built by `build.sh test`,
    // here on this layer's C functions instead of its stand-in C library.
    fn no_locks_counted(_uc: &mut runtime::Uc, _a: &mut Args) -> runtime::Ret {
        runtime::Ret::U32(0)
    }
    cxx();
    ios32::register_function("testc_mutexes_held", no_locks_counted);
    let out = std::env::var("GUEST32_CXX_OUT").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/gamebridge/guest32-cxx/out")
    });
    let path = out.join("test/libcxxtest.so");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guest32-cxx/build.sh test)", path.display()));
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let index = arm32_bridge::elf::open_bytes(uc, "libcxxtest.so", &bytes, true).unwrap();
    let run_all = arm32_bridge::elf::with_loader(|loader| loader.libraries[index].export(uc, "run_all")).unwrap();
    let failed = runtime::call_guest(uc, run_all, &[]).unwrap() as u32;
    assert_eq!(failed, 0, "run_all: one bit per failed check");
}

#[test]
fn exit_runs_static_destructors_first() {
    static RAN_WITH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn destructor(_uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        RAN_WITH.store(a.u32(), std::sync::atomic::Ordering::SeqCst);
        runtime::Ret::Void
    }
    cxx();
    ios32::register_function("test_static_destructor", destructor);
    {
        let _gil = runtime::hold_gil();
        let uc = runtime::engine();
        let function = runtime::stub_for("test_static_destructor");
        runtime::call_guest(uc, guest_export("__cxa_atexit"), &[function, 7, 0]).unwrap();
    }
    call("exit", [3, 0, 0, 0], &[]);
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    assert_eq!(uc.get_data().exit_code, Some(3));
    uc.get_data_mut().exit_code = None;
    assert_eq!(RAN_WITH.load(std::sync::atomic::Ordering::SeqCst), 7, "__cxa_finalize(NULL) ran the destructor");
}

#[test]
fn clocks_use_apples_ids_and_libstdcxx_is_absent() {
    let out = alloc(8);
    // CLOCK_UPTIME_RAW (8; Linux's 8 is CLOCK_REALTIME_ALARM): time since boot.
    assert_eq!(call("clock_gettime", [8, out, 0, 0], &[]).0, 0);
    let uptime = mem().read_u32(out);
    assert_eq!(call("clock_gettime", [0, out, 0, 0], &[]).0, 0);
    assert!(uptime < mem().read_u32(out) / 2, "uptime, not the date");
    // CLOCK_THREAD_CPUTIME_ID is 16 (not a Linux clock id).
    assert_eq!(call("clock_gettime", [16, out, 0, 0], &[]).0, 0);
    // libc++ asks with RTLD_NOLOAD whether GCC's libstdc++ is loaded: it isn't.
    assert_eq!(call("dlopen", [cstring("/usr/lib/libstdc++.6.dylib"), 0x10, 0, 0], &[]).0, 0);
}

#[test]
fn sjlj_slot_stays_the_unwinders() {
    // Slot 18 of the thread block holds each thread's SjLj context list,
    // read and written by guest code: zero at thread start, never a key's.
    for _ in 0..24 {
        let out = alloc(4);
        assert_eq!(call("pthread_key_create", [out, 0, 0, 0], &[]).0, 0);
        assert_eq!(call("pthread_setspecific", [mem().read_u32(out), 0xBAD, 0, 0], &[]).0, 0);
    }
    let _gil = runtime::hold_gil();
    let block = runtime::engine().get_data().tls_block;
    assert_eq!(mem().read_u32(block + 18 * 4), 0);
}

#[test]
fn statically_initialized_mutexes_work() {
    // PTHREAD_RECURSIVE_MUTEX_INITIALIZER: locks twice from one thread.
    let recursive = alloc(44);
    unsafe { std::ptr::write_bytes(mem().host(recursive), 0, 44) };
    mem().write_u32(recursive, 0x32AA_ABA2);
    assert_eq!(call("pthread_mutex_lock", [recursive, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("pthread_mutex_trylock", [recursive, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("pthread_mutex_unlock", [recursive, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("pthread_mutex_unlock", [recursive, 0, 0, 0], &[]).0, 0);
    // PTHREAD_MUTEX_INITIALIZER: free at first, then busy (EBUSY 16) while held.
    let normal = alloc(44);
    unsafe { std::ptr::write_bytes(mem().host(normal), 0, 44) };
    mem().write_u32(normal, 0x32AA_ABA7);
    assert_eq!(call("pthread_mutex_trylock", [normal, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("pthread_mutex_trylock", [normal, 0, 0, 0], &[]).0, 16);
    assert_eq!(call("pthread_mutex_unlock", [normal, 0, 0, 0], &[]).0, 0);
}
