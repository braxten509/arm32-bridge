//! Behavior that could silently break: Darwin layouts, Apple's 64-bit
//! argument rule, errno numbers, static pthread initializers. Each test
//! calls the registered handler the way translated game code would.

use super::libsystem::{RUNE_ALPHA, RUNE_LOWER, RUNE_LOWER_MAP, RUNE_TYPES, RUNE_XDIGIT};
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, ArgSource, Args, GuestCpu};

/// The host layer's symbols and the 32-bit layer's names, registered once
/// for every 32-bit test module.
pub(crate) fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::run::setup();
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

/// Calls the stand-in for `name` as translated code would (with its stand-in name).
fn call_stub(name: &str, registers: [u32; 4], stack: &[u32]) -> (u32, u32) {
    setup();
    let address = runtime::stub_for(name);
    let stub = runtime::stub((address - arm32_bridge::mem::stub_base()) / 8).unwrap();
    let sp = alloc(64 + stack.len() as u32 * 4);
    for (i, word) in stack.iter().enumerate() {
        mem().write_u32(sp + i as u32 * 4, *word);
    }
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    uc.get_data_mut().current_call = stub.name;
    let result = stub.handler.unwrap()(uc, &mut Args::new(registers, sp));
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

#[test]
fn bionic_library_calls_keep_androids_rules() {
    // The built-in libz is built against bionic: its lseek64(fd, off64_t,
    // whence) has the offset in r2:r3 (EABI pair), whence on the stack.
    let base = std::env::temp_dir().join(format!("guest32-bionic-{}", std::process::id()));
    std::fs::create_dir_all(base.join("full/inside")).unwrap();
    let file = base.join("data");
    std::fs::write(&file, [7u8; 2000]).unwrap();
    let fd = call_stub("bionic:open", [cstring(file.to_str().unwrap()), 0, 0, 0], &[]).0;
    assert_eq!(call_stub("bionic:lseek64", [fd, 0xDEAD, 1024, 0], &[0]), (1024, 0));
    call_stub("bionic:close", [fd, 0, 0, 0], &[]);
    // Its errno numbers are Linux's: ENOTEMPTY is 39 (Darwin's is 66).
    assert_eq!(call_stub("bionic:rmdir", [cstring(base.join("full").to_str().unwrap()), 0, 0, 0], &[]).0 as i32, -1);
    assert_eq!(errno(), 39);
    // Handlers that tell calls apart by name see the plain name: feof is not clearerr.
    let stream = call_stub("bionic:fopen", [cstring(file.to_str().unwrap()), cstring("r"), 0, 0], &[]).0;
    call_stub("bionic:fseek", [stream, 0, 2, 0], &[]);
    assert_eq!(call_stub("bionic:fgetc", [stream, 0, 0, 0], &[]).0 as i32, -1);
    assert_ne!(call_stub("bionic:feof", [stream, 0, 0, 0], &[]).0, 0, "at the end of the file");
    call_stub("bionic:fclose", [stream, 0, 0, 0], &[]);
    std::fs::remove_dir_all(&base).unwrap();
    // The program's own (Apple) calls still use Apple's rules.
    let x = 0.25f64.to_bits();
    let out = alloc(16);
    call("__sincos_stret", [out, x as u32, (x >> 32) as u32, 0], &[]);
    assert_eq!(f64::from_bits(mem().read_u64(out)), 0.25f64.sin());
}

#[test]
fn checked_printf_stops_at_the_object_size() {
    // __sprintf_chk(buffer, flags, object size, format, ...): Apple's
    // __chk_fail stops the program instead of writing past the object.
    let buffer = alloc(16);
    mem().write_bytes(buffer, &[0xEE; 16]);
    call("__sprintf_chk", [buffer, 0, 4, cstring("%s")], &[cstring("abcdef")]);
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let fault = uc.get_data_mut().fault.take();
    assert!(fault.is_some_and(|f| f.contains("overflow")), "stopped");
    assert_eq!(mem().bytes(buffer + 4, 12), [0xEE; 12], "nothing written past the object");
    drop(_gil);
    // __snprintf_chk(buffer, max length, flags, object size, format, ...) with max length > object size.
    call("__snprintf_chk", [buffer, 8, 0, 4], &[cstring("%s"), cstring("ab")]);
    let _gil = runtime::hold_gil();
    assert!(runtime::engine().get_data_mut().fault.take().is_some(), "max length larger than the object stops");
}

#[test]
fn streaming_cryptor_round_trip() {
    // AES-128 CBC with PKCS7 padding (options 1), in two pieces each way.
    let (key, iv) = (alloc(16), alloc(16));
    mem().write_bytes(key, &[7; 16]);
    mem().write_bytes(iv, &[9; 16]);
    let plain: Vec<u8> = (0..40u8).collect();
    let input = alloc(48);
    mem().write_bytes(input, &plain);
    let crypt = |decrypt: u32, input: u32, length: u32, out: u32| -> u32 {
        let reference = alloc(4);
        assert_eq!(call("CCCryptorCreate", [decrypt, 0, 1, key], &[16, iv, reference]).0, 0);
        let cryptor = mem().read_u32(reference);
        let moved = alloc(4);
        let half = length / 2;
        assert_eq!(call("CCCryptorUpdate", [cryptor, input, half, out], &[64, moved]).0, 0);
        let mut total = mem().read_u32(moved);
        assert_eq!(call("CCCryptorUpdate", [cryptor, input + half, length - half, out + total], &[64, moved]).0, 0);
        total += mem().read_u32(moved);
        // Too little room for the last piece: kCCBufferTooSmall.
        assert_eq!(call("CCCryptorFinal", [cryptor, out + total, 0, moved], &[]).0 as i32, -4301);
        assert_eq!(call("CCCryptorFinal", [cryptor, out + total, 64, moved], &[]).0, 0);
        total += mem().read_u32(moved);
        assert_eq!(call("CCCryptorRelease", [cryptor, 0, 0, 0], &[]).0, 0);
        total
    };
    let encrypted = alloc(64);
    let length = crypt(0, input, 40, encrypted);
    let expected = crate::fw::crypto::crypt(false, 0, 1, &[7; 16], Some(&[9; 16]), &plain).unwrap();
    assert_eq!(mem().bytes(encrypted, length as usize), &expected[..]);
    let decrypted = alloc(64);
    let length = crypt(1, encrypted, length, decrypted);
    assert_eq!(mem().bytes(decrypted, length as usize), &plain[..]);
}

#[test]
fn cryptor_final_measures_the_unpadded_output() {
    // One plaintext byte, AES-CBC with PKCS7 padding: one 16-byte cipher block.
    let (key, iv) = (alloc(16), alloc(16));
    mem().write_bytes(key, &[7; 16]);
    mem().write_bytes(iv, &[9; 16]);
    let cipher = crate::fw::crypto::crypt(false, 0, 1, &[7; 16], Some(&[9; 16]), &[0x5A]).unwrap();
    let input = alloc(16);
    mem().write_bytes(input, &cipher);
    let decrypt = || {
        let reference = alloc(4);
        assert_eq!(call("CCCryptorCreate", [1, 0, 1, key], &[16, iv, reference]).0, 0);
        let cryptor = mem().read_u32(reference);
        let moved = alloc(4);
        assert_eq!(call("CCCryptorUpdate", [cryptor, input, 16, alloc(16)], &[16, moved]).0, 0);
        assert_eq!(mem().read_u32(moved), 0, "the block is held back for Final");
        (cryptor, moved)
    };
    // Final with room for the one byte it really gives back.
    let (cryptor, moved) = decrypt();
    let out = alloc(4);
    assert_eq!(call("CCCryptorFinal", [cryptor, out, 1, moved], &[]).0, 0);
    assert_eq!((mem().read_u32(moved), mem().read_u8(out)), (1, 0x5A));
    // No room: kCCBufferTooSmall, and the cryptor still has its block for a retry.
    let (cryptor, moved) = decrypt();
    assert_eq!(call("CCCryptorFinal", [cryptor, out, 0, moved], &[]).0 as i32, -4301);
    mem().write_u8(out, 0);
    assert_eq!(call("CCCryptorFinal", [cryptor, out, 16, moved], &[]).0, 0);
    assert_eq!((mem().read_u32(moved), mem().read_u8(out)), (1, 0x5A));
}

/// The C++ runtime loaded once, as `run` loads it (call before holding the GIL).
fn cxx() {
    setup();
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _gil = runtime::hold_gil();
        let uc = runtime::engine();
        super::open_cxx_library(uc).expect("the C++ runtime loads");
        super::objc::exceptions::setup(uc).expect("Objective-C exceptions set up");
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
    // Built next to the embedded library (build.rs).
    let path = std::path::Path::new(env!("GUEST32_CXX_OUT_DIR")).join("test/libcxxtest.so");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guest32-cxx/build.sh test)", path.display()));
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let index = arm32_bridge::elf::open_bytes(uc, "libcxxtest.so", &bytes, true).unwrap();
    let run_all = arm32_bridge::elf::with_loader(|loader| loader.libraries[index].export(uc, "run_all")).unwrap();
    let failed = runtime::call_guest(uc, run_all, &[]).unwrap() as u32;
    assert_eq!(failed, 0, "run_all: one bit per failed check");
}

#[test]
fn cxx_runtime_keeps_nothing_in_r9_across_game_code() {
    // Apple's armv7 code may change r9 in any call (it is not kept), so the
    // library must not keep a value there across a call into game code.
    // TownCraft: libc++'s xsputn kept its source pointer in r9 across the
    // game's own basic_stringbuf::overflow, which changed r9.
    static SCRATCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn overflow(uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        let (this, c) = (a.u32(), a.u32());
        let out = mem().read_u32(this + 0x20);
        mem().write_u8(out, c as u8);
        mem().write_u32(this + 0x18, out + 1); // pptr
        mem().write_u32(this + 0x1c, out + 32); // epptr
        uc.set_reg(9, SCRATCH.load(std::sync::atomic::Ordering::SeqCst) - 1);
        runtime::Ret::U32(c)
    }
    cxx();
    ios32::register_function("test_overflow_changing_r9", overflow);
    SCRATCH.store(cstring("###########################"), std::sync::atomic::Ordering::SeqCst);
    // basic_streambuf<char>: vtable, locale, eback/gptr/egptr, pbase/pptr/epptr
    // (all 0: the first character goes to overflow); +0x20 here: the output.
    let (vtable, buffer, out) = (alloc(0x40), alloc(0x24), alloc(64));
    for offset in (0..0x40).step_by(4) {
        mem().write_u32(vtable + offset, 0);
    }
    mem().write_u32(vtable + 0x34, runtime::stub_for("test_overflow_changing_r9"));
    for offset in (0..0x20).step_by(4) {
        mem().write_u32(buffer + offset, 0);
    }
    mem().write_u32(buffer, vtable);
    mem().write_u32(buffer + 0x20, out);
    let text = "hello, world";
    let source = cstring(text);
    let xsputn = guest_export("_ZNSt3__115basic_streambufIcNS_11char_traitsIcEEE6xsputnEPKci");
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let written = runtime::call_guest(uc, xsputn, &[buffer, source, text.len() as u32]).unwrap() as u32;
    assert_eq!(written, text.len() as u32);
    assert_eq!(mem().bytes(out, text.len()), text.as_bytes());
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

/// Each name is registered once across all modules: a second registration
/// silently replaces the first, so the winner would depend on the order in
/// `register_all`.
#[test]
fn register_all_names_each_function_and_data_once() {
    setup();
    super::REGISTERED.with(|names| names.borrow_mut().clear());
    super::register_all();
    let names = super::REGISTERED.with(|names| std::mem::take(&mut *names.borrow_mut()));
    assert!(names.len() > 100, "registrations were not recorded");
    let mut seen = std::collections::HashSet::new();
    let mut twice: Vec<String> =
        names.iter().filter(|(_, name)| !seen.insert(name.as_str())).map(|(kind, name)| format!("{kind} {name}")).collect();
    twice.sort();
    twice.dedup();
    assert!(twice.is_empty(), "registered more than once: {twice:?}");
}

// ----- Objective-C exceptions and dispatch queues -----

/// A host NSException's guest object, through the runner's bridge.
fn guest_exception(name: &str, reason: &str) -> u32 {
    use crate::fw::string::make;
    let class = crate::objc::runtime::class_named("NSException");
    let host = crate::objc::runtime::send(class, "exceptionWithName:reason:userInfo:", &[make(name), make(reason), 0]);
    super::objc::wire::test_bridge().lock().unwrap().guest_object(&super::objc::wire::Translator, host).unwrap()
}

#[test]
fn an_uncaught_objc_exception_runs_the_games_handler_then_stops() {
    static HANDLED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn handler(_uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        HANDLED.store(a.u32(), std::sync::atomic::Ordering::SeqCst);
        runtime::Ret::Void
    }
    cxx();
    let exception = guest_exception("TestException", "thrown on purpose");
    ios32::register_function("test_uncaught_handler", handler);
    let handler = runtime::stub_for("test_uncaught_handler");
    call("NSSetUncaughtExceptionHandler", [handler, 0, 0, 0], &[]);
    assert_eq!(call("NSGetUncaughtExceptionHandler", [0; 4], &[]).0, handler);
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    // objc_exception_throw goes on into the guest's __cxa_throw, which finds no
    // catch and calls the terminate handler the runtime installed.
    let result = runtime::call_guest(uc, runtime::stub_for("objc_exception_throw"), &[exception]);
    uc.get_data_mut().fault = None;
    let error = result.expect_err("an uncaught exception stops the program");
    assert!(error.contains("uncaught Objective-C exception: TestException: thrown on purpose"), "{error}");
    assert_eq!(HANDLED.load(std::sync::atomic::Ordering::SeqCst), exception, "the game's handler ran first, with the exception");
}

#[test]
fn objc_catch_clauses_match_by_class_and_hand_over_the_object() {
    use super::objc::wire::{test_bridge, Translator};
    cxx();
    let exception = guest_exception("Caught", "by class");
    let class_of = |name: &str| test_bridge().lock().unwrap().framework_class(&Translator, name).0;
    let memory = mem();
    let vtable = ios32::data_address("objc_ehtype_vtable").expect("objc_ehtype_vtable") + 8;
    let type_info = |name: &str, class: u32| {
        let at = alloc(12);
        for (i, word) in [vtable, cstring(name), class].iter().enumerate() {
            memory.write_u32(at + 4 * i as u32, *word);
        }
        at
    };
    // What objc_exception_throw throws: { object, its type_info }.
    let record = alloc(16);
    for (i, word) in [exception, vtable, cstring("NSException"), memory.read_u32(exception)].iter().enumerate() {
        memory.write_u32(record + 4 * i as u32, *word);
    }
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    // libc++abi asks a catch clause's type_info through its vtable's slot 4
    // (can_catch), with the thrown type and the thrown object's address.
    let can_catch = memory.read_u32(vtable + 16);
    let mut check = |catch_type: u32, thrown_type: u32| {
        let adjusted = alloc(4);
        memory.write_u32(adjusted, record);
        let caught = runtime::call_guest(uc, can_catch, &[catch_type, thrown_type, adjusted]).unwrap() as u32 & 0xff;
        (caught, memory.read_u32(adjusted))
    };
    let thrown = record + 4;
    assert_eq!(check(ios32::data_address("OBJC_EHTYPE_id").unwrap(), thrown), (1, exception), "@catch (id e) gets the object");
    assert_eq!(check(ios32::data_address("OBJC_EHTYPE_$_NSException").unwrap(), thrown).0, 1);
    assert_eq!(check(type_info("NSObject", class_of("NSObject")), thrown).0, 1, "a superclass's clause catches");
    assert_eq!(check(type_info("NSString", class_of("NSString")), thrown).0, 0, "another class's does not");
    assert_eq!(check(type_info("NSException", 0), thrown).0, 1, "a clause whose class is unknown matches by name");
    // Unwinding through @try code: its personality is the C++ one.
    assert_eq!(guest_export("__objc_personality_v0"), guest_export("__gxx_personality_sj0"));
    // A C++ exception is not an Objective-C one.
    let cxx_thrown = guest_export("_ZTISt9exception");
    assert_eq!(check(ios32::data_address("OBJC_EHTYPE_id").unwrap(), cxx_thrown), (0, record));
}

#[test]
fn dispatch_runs_guest_blocks_once_and_in_queue_order() {
    use super::objc::bridge::{dispatch_from_host, set_block_owner, Bridge, Special};
    use super::objc::functions::{call_function, Ctx};
    use super::objc::{mirror, testing::Fake};
    use std::sync::Mutex;
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
        call_function(&ctx, name, &w).unwrap() as u32
    };
    // void (^)(void) blocks capturing a number; each run notes it and its thread.
    let runs: &'static Mutex<Vec<(u32, std::thread::ThreadId)>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    g.code(0x5001, move |w| {
        runs.lock().unwrap().push((g.u32_at(w[0] + 20), std::thread::current().id()));
        0
    });
    let descriptor = g.words(&[0, 24, g.text("v4@?0")]);
    let block = |n: u32| g.words(&[stack_isa, 1 << 30, 0, 0x5001, descriptor, n]);
    let numbers = || runs.lock().unwrap().iter().map(|r| r.0).collect::<Vec<_>>();

    // dispatch_once: Apple's 32-bit predicate is one word (the next stays as it was).
    let predicate = g.words(&[0, 0x1234_5678]);
    call("dispatch_once", &[predicate, block(1)]);
    call("dispatch_once", &[predicate, block(2)]);
    assert_eq!(numbers(), [1]);
    assert_eq!((g.u32_at(predicate), g.u32_at(predicate + 4)), (!0, 0x1234_5678));

    // A serial queue runs async blocks in order on its own thread; sync waits its turn.
    let queue = call("dispatch_queue_create", &[g.text("test"), 0]);
    assert_ne!(queue, 0);
    for n in 10..13 {
        call("dispatch_async", &[queue, block(n)]);
    }
    call("dispatch_sync", &[queue, block(13)]);
    assert_eq!(numbers(), [1, 10, 11, 12, 13]);
    let me = std::thread::current().id();
    assert!(runs.lock().unwrap()[1..4].iter().all(|r| r.1 != me), "async blocks ran on the queue's thread");
    call("dispatch_release", &[queue]);
    // A global queue's sync block runs before dispatch_sync returns.
    let global = call("dispatch_get_global_queue", &[0, 0]);
    assert_ne!(global, 0);
    call("dispatch_sync", &[global, block(20)]);
    assert_eq!(numbers().last(), Some(&20));
    // dispatch_get_main_queue() is &_dispatch_main_q: that address is the host's main queue.
    let main = ios32::data_address("_dispatch_main_q").expect("_dispatch_main_q");
    assert_eq!(super::fw::handle_to_host(main), crate::symbols::lookup("__dispatch_main_q").unwrap().0);
}
