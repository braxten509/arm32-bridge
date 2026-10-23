//! Behavior that could silently break: Darwin layouts, Apple's 64-bit
//! argument rule, errno numbers, static pthread initializers. Each test
//! calls the registered handler the way translated game code would.

use super::libsystem::{RUNE_ALPHA, RUNE_LOWER, RUNE_LOWER_MAP, RUNE_TYPES, RUNE_XDIGIT};
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, Args, GuestCpu};

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
