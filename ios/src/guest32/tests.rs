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

/// Data bytes come from the guest heap while the first part is held, as
/// in a 32-bit game's run; the second keeps the tests that need it apart
/// (it goes last).
pub(crate) fn guest_data_bytes() -> (crate::fw::data::HeapHeld, std::sync::MutexGuard<'static, ()>) {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    setup();
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    (super::fw::keep_data_bytes_in_guest_memory(), serial)
}

pub(crate) fn alloc(size: u32) -> u32 {
    setup();
    super::alloc(size)
}

pub(crate) fn cstring(text: &str) -> u32 {
    setup();
    super::cstring(text)
}

/// Calls registered function `name` with r0-r3 and stack words; returns (r0, r1).
pub(crate) fn call(name: &str, registers: [u32; 4], stack: &[u32]) -> (u32, u32) {
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
fn host_threads_give_their_guest_stack_back_even_when_android_clears_the_slot_first() {
    // SpellCraft on the Pixel: every dispatch worker took a new 1 MB guest
    // stack and none came back (Android cleared the engine slot before the
    // thread-end hook read it), so after ~247 workers the game stopped with
    // "out of guest stack space". More workers than there is room for must run.
    setup();
    for round in 0..300 {
        let ran = std::thread::spawn(|| {
            let _gil = runtime::hold_gil();
            runtime::engine();
            runtime::lose_engine_slot();
        })
        .join();
        assert!(ran.is_ok(), "worker {round} could not get a guest stack");
    }
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
fn statfs_and_statvfs_fill_darwins_layouts() {
    // Darwin armv7 (64-bit inodes): struct statfs is 2168 bytes with 64-bit
    // block counts from 8; struct statvfs is 44 bytes with 32-bit ones.
    let folder = std::env::temp_dir();
    let host = {
        let path = std::ffi::CString::new(folder.to_str().unwrap()).unwrap();
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::statvfs(path.as_ptr(), &mut stats) }, 0);
        stats
    };
    let host_total = host.f_blocks as u64 * host.f_frsize as u64;
    let close = |bytes: u64| bytes.abs_diff(host_total) <= host_total / 100;
    let out = alloc(2168 + 4);
    mem().write_u32(out + 2168, 0xFEED_F00D);
    assert_eq!(call("statfs", [cstring(folder.to_str().unwrap()), out, 0, 0], &[]).0, 0);
    let block = mem().read_u32(out) as u64;
    assert!(block > 0 && close(mem().read_u64(out + 8) * block), "f_bsize at 0 and f_blocks at 8 give the disk size");
    assert!(mem().read_u64(out + 24) <= mem().read_u64(out + 8), "f_bavail at 24");
    assert_eq!(mem().string(out + 72), "hfs", "f_fstypename at 72");
    assert_eq!(mem().string(out + 88), "/private/var", "f_mntonname at 88");
    assert_eq!(mem().read_u32(out + 2168), 0xFEED_F00D, "nothing past 2168 bytes");
    let fd = unsafe { libc::open(std::ffi::CString::new(folder.to_str().unwrap()).unwrap().as_ptr(), libc::O_RDONLY) };
    assert_eq!(call("fstatfs", [fd as u32, out, 0, 0], &[]).0, 0);
    unsafe { libc::close(fd) };
    assert_eq!(mem().string(out + 72), "hfs");

    let out = alloc(44 + 4);
    mem().write_u32(out + 44, 0xFEED_F00D);
    assert_eq!(call("statvfs", [cstring(folder.to_str().unwrap()), out, 0, 0], &[]).0, 0);
    let fragment = mem().read_u32(out + 4) as u64;
    assert!(fragment > 0 && close(mem().read_u32(out + 8) as u64 * fragment), "f_frsize at 4 and f_blocks at 8");
    assert!(mem().read_u32(out + 16) <= mem().read_u32(out + 8), "f_bavail at 16");
    assert_eq!(mem().read_u32(out + 44), 0xFEED_F00D, "nothing past 44 bytes");
    assert_eq!(call("statvfs", [cstring("/no/such/folder"), out, 0, 0], &[]).0 as i32, -1);
    assert_eq!(errno(), 2, "ENOENT");
}

#[test]
fn directories_list_with_darwins_dirent() {
    // struct dirent (64-bit inodes): d_ino 0, d_seekoff 8, d_reclen 16,
    // d_namlen 18, d_type 20, d_name 21 (1024 bytes).
    let folder = std::env::temp_dir().join(format!("guest32-dir-{}", std::process::id()));
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    std::fs::write(folder.join("a.txt"), b"x").unwrap();
    let dir = call("opendir", [cstring(folder.to_str().unwrap()), 0, 0, 0], &[]).0;
    assert_ne!(dir, 0);
    let read_all = || {
        let mut names = Vec::new();
        loop {
            let entry = call("readdir", [dir, 0, 0, 0], &[]).0;
            if entry == 0 {
                break;
            }
            let name = mem().string(entry + 21);
            assert_eq!(mem().read_u16(entry + 18) as usize, name.len(), "d_namlen");
            assert!(mem().read_u16(entry + 16) as usize >= 21 + name.len() + 1, "d_reclen");
            assert_ne!(mem().read_u64(entry), 0, "d_ino");
            names.push((name, mem().read_u8(entry + 20)));
        }
        names.sort();
        names
    };
    let expected = vec![(".".to_string(), 4), ("..".to_string(), 4), ("a.txt".to_string(), 8), ("sub".to_string(), 4)];
    assert_eq!(read_all(), expected, "DT_DIR 4, DT_REG 8");
    call("rewinddir", [dir, 0, 0, 0], &[]);
    let (entry, result) = (alloc(1048), alloc(4));
    assert_eq!(call("readdir_r", [dir, entry, result, 0], &[]).0, 0);
    assert_eq!(mem().read_u32(result), entry, "readdir_r fills the caller's entry");
    assert_eq!(read_all().len(), 3, "the rest after rewinding");
    assert_eq!(call("readdir_r", [dir, entry, result, 0], &[]).0, 0);
    assert_eq!(mem().read_u32(result), 0, "NULL at the end");
    assert_eq!(call("closedir", [dir, 0, 0, 0], &[]).0, 0);
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(call("opendir", [cstring("/no/such/folder"), 0, 0, 0], &[]).0, 0);
    assert_eq!(errno(), 2, "ENOENT");
}

#[test]
fn packed_byte_extends_translate() {
    // UXTB16/SXTB16/UXTAB16/SXTAB16, Thumb-2 and ARM: bytes 0 and 2 of the
    // (rotated) Rm become halfwords, added halfword by halfword to Rn.
    setup();
    let thumb = |instruction: [u16; 2]| {
        let address = alloc(8);
        for (i, half) in [instruction[0], instruction[1], 0x4770].iter().enumerate() {
            mem().write_bytes(address + i as u32 * 2, &half.to_le_bytes()); // ...; bx lr
        }
        address | 1
    };
    let arm = |instruction: u32| {
        let address = alloc(8);
        mem().write_u32(address, instruction);
        mem().write_u32(address + 4, 0xE12F_FF1E); // bx lr
        address
    };
    let (x, n) = (0x80F1_7F82u32, 0x0001_FFFFu32);
    let cases = [
        ("uxtb16 r0, r0", [0xFA3F, 0xF080], 0xE6CF_0070, 0x00F1_0082u32),
        ("sxtb16 r0, r0", [0xFA2F, 0xF080], 0xE68F_0070, 0xFFF1_FF82),
        ("uxtab16 r0, r1, r0", [0xFA31, 0xF080], 0xE6C1_0070, 0x00F2_0081),
        ("sxtab16 r0, r1, r0", [0xFA21, 0xF080], 0xE681_0070, 0xFFF2_FF81),
        ("uxtb16 r0, r0, ror #8", [0xFA3F, 0xF090], 0xE6CF_0470, 0x0080_007F),
    ];
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    for (name, thumb_form, arm_form, expected) in cases {
        let got = runtime::call_guest(uc, thumb(thumb_form), &[x, n]).unwrap() as u32;
        assert_eq!(got, expected, "Thumb {name}: {got:#010x}");
        let got = runtime::call_guest(uc, arm(arm_form), &[x, n]).unwrap() as u32;
        assert_eq!(got, expected, "ARM {name}: {got:#010x}");
    }
}

#[test]
fn fmax_and_fmin_take_doubles_and_skip_nan() {
    // Doubles in r0:r1 and r2:r3 (softfp), the result in r0:r1.
    let both = |name: &str, x: f64, y: f64| {
        let (x, y) = (x.to_bits(), y.to_bits());
        let (low, high) = call(name, [x as u32, (x >> 32) as u32, y as u32, (y >> 32) as u32], &[]);
        f64::from_bits(low as u64 | (high as u64) << 32)
    };
    assert_eq!(both("fmax", 1.5, -2.0), 1.5);
    assert_eq!(both("fmin", 1.5, -2.0), -2.0);
    assert_eq!(both("fmax", f64::NAN, 3.0), 3.0, "NaN loses");
    assert_eq!(both("fmin", -4.0, f64::NAN), -4.0);
}

#[test]
fn system_has_no_shell_as_in_the_ios_sandbox() {
    assert_eq!(call("system", [0; 4], &[]).0, 0, "system(NULL): no command processor");
    assert_eq!(call("system", [cstring("ls"), 0, 0, 0], &[]).0 as i32, -1);
    assert_eq!(errno(), 1, "EPERM: apps may not start processes");
}

#[test]
fn uname_fills_darwins_five_256_byte_fields() {
    let names = alloc(5 * 256);
    assert_eq!(call("uname", [names, 0, 0, 0], &[]).0, 0);
    assert_eq!(mem().string(names), "Darwin", "sysname");
    assert!(mem().string(names + 2 * 256).starts_with("14."), "release at 512");
    assert!(mem().string(names + 3 * 256).starts_with("Darwin Kernel Version"), "version at 768");
    assert_eq!(mem().string(names + 4 * 256), crate::libsystem::io::device_model(), "machine at 1024, as hw.machine");
}

#[test]
fn scheduling_uses_darwins_policies_and_priorities() {
    // Darwin: SCHED_OTHER 1, SCHED_RR 2, SCHED_FIFO 4; priorities 15-47 for
    // every policy, 31 by default; struct sched_param { int priority; char opaque[4]; }.
    for policy in [1, 2, 4] {
        assert_eq!(call("sched_get_priority_min", [policy, 0, 0, 0], &[]).0, 15);
        assert_eq!(call("sched_get_priority_max", [policy, 0, 0, 0], &[]).0, 47);
    }
    let (attributes, policy, parameters) = (alloc(40), alloc(4), alloc(8));
    assert_eq!(call("pthread_attr_init", [attributes, 0, 0, 0], &[]).0, 0);
    call("pthread_attr_getschedpolicy", [attributes, policy, 0, 0], &[]);
    call("pthread_attr_getschedparam", [attributes, parameters, 0, 0], &[]);
    assert_eq!((mem().read_u32(policy), mem().read_u32(parameters)), (1, 31), "SCHED_OTHER at 31 by default");
    assert_eq!(call("pthread_attr_setschedpolicy", [attributes, 4, 0, 0], &[]).0, 0);
    assert_eq!(call("pthread_attr_setschedpolicy", [attributes, 3, 0, 0], &[]).0, 22, "no policy 3: EINVAL");
    mem().write_u32(parameters, 40);
    assert_eq!(call("pthread_attr_setschedparam", [attributes, parameters, 0, 0], &[]).0, 0);
    mem().write_u32(parameters, 0);
    call("pthread_attr_getschedpolicy", [attributes, policy, 0, 0], &[]);
    call("pthread_attr_getschedparam", [attributes, parameters, 0, 0], &[]);
    assert_eq!((mem().read_u32(policy), mem().read_u32(parameters)), (4, 40));

    let (me, _) = call("pthread_self", [0; 4], &[]);
    assert_eq!(call("pthread_getschedparam", [me, policy, parameters, 0], &[]).0, 0);
    assert_eq!((mem().read_u32(policy), mem().read_u32(parameters)), (1, 31), "a normal host thread");
    mem().write_u32(parameters, 46);
    assert_eq!(call("pthread_setschedparam", [me, 2, parameters, 0], &[]).0, 0);
    mem().write_u32(parameters, 50);
    assert_eq!(call("pthread_setschedparam", [me, 2, parameters, 0], &[]).0, 22, "above 47: EINVAL");
    assert_eq!(call("pthread_getschedparam", [me, policy, parameters, 0], &[]).0, 0);
    assert_eq!((mem().read_u32(policy), mem().read_u32(parameters)), (2, 46), "what the thread was given");
    // A thread that doesn't exist (or has ended): ESRCH, nothing read or kept.
    let gone = 0x7FFF_FFF0;
    mem().write_u32(policy, 0xAAAA);
    assert_eq!(call("pthread_getschedparam", [gone, policy, parameters, 0], &[]).0, 3, "ESRCH");
    assert_eq!(mem().read_u32(policy), 0xAAAA, "left alone");
    mem().write_u32(parameters, 40);
    assert_eq!(call("pthread_setschedparam", [gone, 2, parameters, 0], &[]).0, 3, "ESRCH");
}

#[test]
fn mbrtowc_keeps_a_partial_character_in_its_state() {
    // UTF-8 é (C3 A9) a byte at a time: (size_t)-2 with the first byte kept
    // in the caller's mbstate_t (Darwin's is 128 bytes), then é from the
    // second, counting only the byte it read.
    let (out, state, text) = (alloc(4), alloc(128), alloc(4));
    // A new mbstate_t starts zeroed, as C code makes it (malloc may hand back
    // memory another test used).
    mem().write_bytes(state, &[0; 128]);
    mem().write_bytes(text, &[0xC3, 0xA9, 0x41, 0]);
    assert_eq!(call("mbrtowc", [out, text, 1, state], &[]).0, u32::MAX - 1, "incomplete");
    assert_ne!(mem().read_u32(state), 0, "the first byte is kept");
    assert_eq!(call("mbrtowc", [out, text + 1, 3, state], &[]).0, 1);
    assert_eq!(mem().read_u32(out), 0xE9, "é");
    assert_eq!(mem().read_u32(state), 0, "back to the start");
    assert_eq!(call("mbrtowc", [out, text + 2, 2, state], &[]).0, 1);
    assert_eq!(mem().read_u32(out), 0x41);
    // A NULL state is the function's own.
    assert_eq!(call("mbrtowc", [out, text, 1, 0], &[]).0, u32::MAX - 1);
    assert_eq!(call("mbrtowc", [out, text + 1, 1, 0], &[]).0, 1);
    assert_eq!(mem().read_u32(out), 0xE9);
    // A byte that can't follow what was kept: EILSEQ, and the state starts over.
    assert_eq!(call("mbrtowc", [out, text, 1, state], &[]).0, u32::MAX - 1);
    assert_eq!(call("mbrtowc", [out, text + 2, 1, state], &[]).0, u32::MAX);
    assert_eq!(errno(), 92, "Darwin's EILSEQ");
    assert_eq!(mem().read_u32(state), 0);
    // mbrlen keeps a state the same way.
    assert_eq!(call("mbrlen_l", [text, 1, state, 0], &[]).0, u32::MAX - 1);
    assert_eq!(call("mbrlen_l", [text + 1, 1, state, 0], &[]).0, 1);
}

#[test]
fn poll_and_ioctl_use_darwins_numbers() {
    // GNU libstdc++'s filebuf asks ioctl(FIONREAD) and poll how much it can read.
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::write(fds[1], b"abc".as_ptr().cast(), 3) }, 3);
    let count = alloc(4);
    assert_eq!(call("ioctl", [fds[0] as u32, 0x4004_667F, count, 0], &[]).0, 0, "Darwin's FIONREAD");
    assert_eq!(mem().read_u32(count), 3);
    assert_eq!(call("ioctl", [fds[0] as u32, 0x4004_7400, count, 0], &[]).0 as i32, -1);
    assert_eq!(errno(), 25, "ENOTTY for a request it doesn't know");
    // { fd, events, revents } x2: POLLIN|POLLWRBAND (Darwin 0x100) and POLLOUT.
    let list = alloc(16);
    for (i, (fd, events)) in [(fds[0], 0x101u32), (fds[1], 0x4)].into_iter().enumerate() {
        mem().write_u32(list + i as u32 * 8, fd as u32);
        mem().write_u32(list + i as u32 * 8 + 4, events);
    }
    assert_eq!(call("poll", [list, 2, 0, 0], &[]).0, 2);
    assert_eq!(mem().read_u16(list + 6), 0x1, "readable");
    assert_eq!(mem().read_u16(list + 14), 0x4, "writable: Darwin's POLLOUT, not Linux's POLLOUT|POLLWRNORM");
    unsafe { libc::close(fds[0]) };
    unsafe { libc::close(fds[1]) };
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

#[test]
fn a_reset_cryptor_starts_again_from_the_new_iv() {
    // AES-128 CBC, no padding: one block, then a reset to another IV, then the same block.
    let (key, iv, other_iv) = (alloc(16), alloc(16), alloc(16));
    mem().write_bytes(key, &[7; 16]);
    mem().write_bytes(iv, &[9; 16]);
    mem().write_bytes(other_iv, &[3; 16]);
    let (input, out, moved, reference) = (alloc(16), alloc(16), alloc(4), alloc(4));
    mem().write_bytes(input, &[0x42; 16]);
    assert_eq!(call("CCCryptorCreate", [0, 0, 0, key], &[16, iv, reference]).0, 0);
    let cryptor = mem().read_u32(reference);
    assert_eq!(call("CCCryptorUpdate", [cryptor, input, 16, out], &[16, moved]).0, 0);
    // Reset with a new IV: the chain starts there (pending input is dropped too).
    assert_eq!(call("CCCryptorUpdate", [cryptor, input, 5, out], &[16, moved]).0, 0);
    assert_eq!(call("CCCryptorReset", [cryptor, other_iv, 0, 0], &[]).0, 0);
    assert_eq!(call("CCCryptorUpdate", [cryptor, input, 16, out], &[16, moved]).0, 0);
    let expected = crate::fw::crypto::crypt(false, 0, 0, &[7; 16], Some(&[3; 16]), &[0x42; 16]).unwrap();
    assert_eq!((mem().read_u32(moved), mem().bytes(out, 16)), (16, &expected[..]));
    // A NULL IV is all zeros.
    assert_eq!(call("CCCryptorReset", [cryptor, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("CCCryptorUpdate", [cryptor, input, 16, out], &[16, moved]).0, 0);
    let expected = crate::fw::crypto::crypt(false, 0, 0, &[7; 16], Some(&[0; 16]), &[0x42; 16]).unwrap();
    assert_eq!(mem().bytes(out, 16), &expected[..]);
    assert_eq!(call("CCCryptorReset", [alloc(4), 0, 0, 0], &[]).0 as i32, -4300, "not a cryptor: kCCParamError");
    call("CCCryptorRelease", [cryptor, 0, 0, 0], &[]);
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
fn cxx_runtime_gnu_checks_pass_on_this_c_library() {
    // guest32-cxx/test/gnutest.cc: GCC 4.2.1's libstdc++ as GCC-era games
    // inline it (copy-on-write strings, string and file streams, locale,
    // exceptions caught by libc++ code, list and map), on this layer's C functions.
    cxx();
    let path = std::path::Path::new(env!("GUEST32_CXX_OUT_DIR")).join("test/libgnutest.so");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guest32-cxx/build.sh test)", path.display()));
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let index = arm32_bridge::elf::open_bytes(uc, "libgnutest.so", &bytes, true).unwrap();
    let run_all = arm32_bridge::elf::with_loader(|loader| loader.libraries[index].export(uc, "gnu_run_all")).unwrap();
    let failed = runtime::call_guest(uc, run_all, &[]).unwrap() as u32;
    assert_eq!(failed, 0, "gnu_run_all: one bit per failed check");
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

/// What a program's import of `name` binds to (`ios32::load`: guest
/// libraries' exports first, then this layer's functions).
fn program_import(name: &str) -> u32 {
    let export = runtime::shared().guest_exports.get(name).copied();
    export.unwrap_or_else(|| {
        assert!(ios32::provides_function(name), "nothing provides {name}");
        runtime::stub_for(name)
    })
}

#[test]
fn zlib_keeps_nothing_in_r9_across_game_callbacks() {
    // Android's libz keeps values in r9 across the game's callbacks:
    // deflateInit2_ its memLevel across zalloc, inflate's updatewindow the
    // output end across zalloc, inflateBack its output pointer across in and
    // out. Apple's armv7 code may change r9 in any call; these stand-ins do.
    // 24 as memLevel asks for more memory than zalloc gives (Z_MEM_ERROR).
    const CHANGED_R9: u32 = 24;
    fn zalloc(uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        let (_opaque, items, size) = (a.u32(), a.u32(), a.u32());
        let bytes = items as u64 * size as u64;
        let address = if bytes > 1 << 24 { 0 } else { super::alloc(bytes as u32) };
        if address != 0 {
            mem().write_bytes(address, &vec![0; bytes as usize]);
        }
        uc.set_reg(9, CHANGED_R9);
        runtime::Ret::U32(address)
    }
    fn zfree(uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        let (_opaque, address) = (a.u32(), a.u32());
        super::free(address);
        uc.set_reg(9, CHANGED_R9);
        runtime::Ret::Void
    }
    // in(descriptor {data, length}, &next): all the input at once.
    fn input(uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        let (descriptor, next) = (a.u32(), a.u32());
        let length = mem().read_u32(descriptor + 4);
        mem().write_u32(next, mem().read_u32(descriptor));
        mem().write_u32(descriptor + 4, 0);
        uc.set_reg(9, CHANGED_R9);
        runtime::Ret::U32(length)
    }
    // out(descriptor {output, written}, data, length): appended.
    fn output(uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        let (descriptor, data, length) = (a.u32(), a.u32(), a.u32());
        let written = mem().read_u32(descriptor + 4);
        let bytes = mem().bytes(data, length as usize).to_vec();
        mem().write_bytes(mem().read_u32(descriptor) + written, &bytes);
        mem().write_u32(descriptor + 4, written + length);
        uc.set_reg(9, CHANGED_R9);
        runtime::Ret::U32(0)
    }
    setup();
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _gil = runtime::hold_gil();
        super::zlib::open(runtime::engine()).expect("libz loads");
    });
    let handlers: [(&str, runtime::Handler); 4] =
        [("test_zalloc_changing_r9", zalloc), ("test_zfree_changing_r9", zfree), ("test_in_changing_r9", input), ("test_out_changing_r9", output)];
    for (name, handler) in handlers {
        ios32::register_function(name, handler);
    }
    let text: Vec<u8> = (0..20000u32).map(|i| b"zlib keeps r9? "[(i % 15) as usize] ^ (i / 997) as u8).collect();
    let length = text.len() as u32;
    let plain = alloc(length);
    mem().write_bytes(plain, &text);
    let version = cstring("1.2.11");
    // z_stream (56 bytes): next_in 0, avail_in 4, next_out 0xc, avail_out 0x10, total_out 0x14, zalloc 0x20, zfree 0x24.
    let stream = || {
        let stream = alloc(56);
        mem().write_bytes(stream, &[0; 56]);
        mem().write_u32(stream + 0x20, runtime::stub_for("test_zalloc_changing_r9"));
        mem().write_u32(stream + 0x24, runtime::stub_for("test_zfree_changing_r9"));
        stream
    };
    let (deflating, inflating, back) = (stream(), stream(), stream());
    let (packed, unpacked, unpacked_back, window) = (alloc(length + 64), alloc(length), alloc(length), alloc(1 << 15));
    let (input_descriptor, output_descriptor) = (alloc(8), alloc(8));
    let (input, output) = (runtime::stub_for("test_in_changing_r9"), runtime::stub_for("test_out_changing_r9"));
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let mut zlib = |name: &str, args: &[u32]| runtime::call_guest(uc, program_import(name), args).unwrap() as u32 as i32;
    let raw = (-15i32) as u32;

    assert_eq!(zlib("deflateInit2_", &[deflating, 6, 8, raw, 8, 0, version, 56]), 0, "deflateInit2_ (-4: memLevel lost)");
    for (offset, value) in [(0, plain), (4, length), (0xc, packed), (0x10, length + 64)] {
        mem().write_u32(deflating + offset, value);
    }
    assert_eq!(zlib("deflate", &[deflating, 4]), 1, "deflate finishes");
    let packed_length = mem().read_u32(deflating + 0x14);
    assert_eq!(zlib("deflateEnd", &[deflating]), 0);

    // In 1000-byte pieces: inflate keeps a window (updatewindow).
    assert_eq!(zlib("inflateInit2_", &[inflating, raw, version, 56]), 0);
    mem().write_u32(inflating, packed);
    mem().write_u32(inflating + 4, packed_length);
    let mut result = 0;
    for _ in 0..100 {
        let done = mem().read_u32(inflating + 0x14);
        mem().write_u32(inflating + 0xc, unpacked + done);
        mem().write_u32(inflating + 0x10, (length - done).min(1000));
        result = zlib("inflate", &[inflating, 0]);
        if result != 0 {
            break;
        }
    }
    assert_eq!(result, 1, "inflate reaches the end");
    assert!(mem().bytes(unpacked, text.len()) == text, "inflate gives the text back");
    assert_eq!(zlib("inflateEnd", &[inflating]), 0);

    assert_eq!(zlib("inflateBackInit_", &[back, 15, window, version, 56]), 0);
    mem().write_u32(input_descriptor, packed);
    mem().write_u32(input_descriptor + 4, packed_length);
    mem().write_u32(output_descriptor, unpacked_back);
    mem().write_u32(output_descriptor + 4, 0);
    assert_eq!(zlib("inflateBack", &[back, input, input_descriptor, output, output_descriptor]), 1, "inflateBack reaches the end");
    assert_eq!(mem().read_u32(output_descriptor + 4), length);
    assert!(mem().bytes(unpacked_back, text.len()) == text, "inflateBack gives the text back");
    assert_eq!(zlib("inflateBackEnd", &[back]), 0);
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

#[test]
fn block_classes_are_replaced_before_anything_binds_them() {
    use super::objc::bridge::Special;
    use super::objc::wire::{replace_block_classes, test_bridge};
    setup();
    let _ = test_bridge();
    // run(): register_all, then register_program replaces libSystem's block
    // classes, then the libraries' constructors run and the program binds.
    replace_block_classes().expect("nothing has bound the block classes yet");
    let isa = ios32::data_address("_NSConcreteStackBlock").unwrap();
    assert_eq!(test_bridge().lock().unwrap().special(isa), Some(Special::StackBlock));
    // Once bound (data addresses are kept), a late replacement would be
    // ignored: the check says so instead.
    let error = replace_block_classes().expect_err("a replacement after binding is refused");
    assert!(error.contains("_NSConcreteStackBlock"), "{error}");
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
    // Every exception the handler saw: it is the whole program's, so other
    // tests throwing meanwhile reach it too.
    static HANDLED: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
    fn handler(_uc: &mut runtime::Uc, a: &mut Args) -> runtime::Ret {
        HANDLED.lock().unwrap().push(a.u32());
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
    let handled = HANDLED.lock().unwrap().clone();
    assert!(handled.contains(&exception), "the game's handler ran first, with the exception {exception:#x}: {handled:x?}");
}

#[test]
fn raise_and_raise_format_from_game_code_throw_through_the_guest_runtime() {
    use super::objc::wire::{test_bridge, Translator};
    use crate::fw::string::make;
    cxx();
    let exception = guest_exception("RaisedException", "raised on purpose");
    let (raise, raise_format, class, name, format) = {
        let mut b = test_bridge().lock().unwrap();
        (
            b.guest_selector(&Translator, "raise"),
            b.guest_selector(&Translator, "raise:format:"),
            b.framework_class(&Translator, "NSException").0,
            b.guest_object(&Translator, make("FormatException")).unwrap(),
            b.guest_object(&Translator, make("value %d of %s")).unwrap(),
        )
    };
    let text = cstring("ten");
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let send = runtime::stub_for("objc_msgSend");
    // [exception raise]: thrown as objc_exception_throw throws it (nothing
    // catches it here, so it ends at the terminate handler).
    let result = runtime::call_guest(uc, send, &[exception, raise]);
    uc.get_data_mut().fault = None;
    let error = result.expect_err("an uncaught exception stops the program");
    assert!(error.contains("uncaught Objective-C exception: RaisedException: raised on purpose"), "{error}");
    // [NSException raise:@"FormatException" format:@"value %d of %s", 42, "ten"]
    let result = runtime::call_guest(uc, send, &[class, raise_format, name, format, 42, text]);
    uc.get_data_mut().fault = None;
    let error = result.expect_err("an uncaught exception stops the program");
    assert!(error.contains("uncaught Objective-C exception: FormatException: value 42 of ten"), "{error}");
}

/// An exception host code raises while answering a game's message (here
/// key-value coding's NSUnknownKeyException) is thrown in the game, where
/// its @catch could take it; uncaught, it ends at the terminate handler.
#[test]
fn host_raises_reach_game_code_as_game_throws() {
    use super::objc::wire::{test_bridge, Translator};
    use crate::fw::string::make;
    cxx();
    let (object, value_for_key, key) = {
        let mut b = test_bridge().lock().unwrap();
        let host = crate::objc::runtime::send(crate::objc::runtime::send(crate::objc::runtime::class_named("NSObject"), "alloc", &[]), "init", &[]);
        (b.guest_object(&Translator, host).unwrap(), b.guest_selector(&Translator, "valueForKey:"), b.guest_object(&Translator, make("nope")).unwrap())
    };
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let result = runtime::call_guest(uc, runtime::stub_for("objc_msgSend"), &[object, value_for_key, key]);
    uc.get_data_mut().fault = None;
    let error = result.expect_err("the raise reaches the game");
    assert!(error.contains("uncaught Objective-C exception: NSUnknownKeyException"), "{error}");
}

/// Warm barriers reuse their constant personality without locking the
/// stub table, while each nested call keeps its own return trap and link.
#[test]
fn warm_exception_barriers_do_not_relock_the_stub_table() {
    use super::objc::exceptions::{Barrier, BARRIER};
    setup();
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let personality = runtime::stub_for(BARRIER);
    let top = uc.get_data().tls_block + 18 * 4;
    let original_top = mem().read_u32(top);
    let original_depth = uc.get_data().depth;
    // Warm both reusable contexts before holding the shared stub table.
    let outer = Barrier::push(uc);
    let inner = Barrier::push(uc);
    assert_eq!(inner.pop(), None);
    assert_eq!(outer.pop(), None);
    // shared() rejects a second acquisition by this thread. Repeated
    // barriers must use the resolved personality without any table lookup.
    let shared = runtime::shared();
    for depth in 0..3 {
        uc.get_data_mut().depth = depth;
        let outer = Barrier::push(uc);
        let outer_at = mem().read_u32(top);
        assert_eq!(mem().read_u32(outer_at + 24), personality);
        assert_eq!(mem().read_u32(outer_at + 36), runtime::next_return_trap(uc));
        uc.get_data_mut().depth = depth + 1;
        let inner = Barrier::push(uc);
        let inner_at = mem().read_u32(top);
        assert_ne!(inner_at, outer_at);
        assert_eq!(mem().read_u32(inner_at), outer_at);
        assert_eq!(mem().read_u32(inner_at + 24), personality);
        assert_eq!(mem().read_u32(inner_at + 36), runtime::next_return_trap(uc));
        assert_eq!(inner.pop(), None);
        assert_eq!(mem().read_u32(top), outer_at);
        assert_eq!(outer.pop(), None);
        assert_eq!(mem().read_u32(top), original_top);
    }
    drop(shared);
    uc.get_data_mut().depth = original_depth;
}

/// A game exception escaping game code that host code called (game → host
/// → game) stops at that call instead of unwinding past the host's frames
/// into an outer game frame: the host gets it as a raise, which the bridge
/// throws again in the game code that called the host.
#[test]
fn game_exceptions_stop_at_the_host_call_they_escape() {
    use super::objc::bridge::GuestSide;
    use super::objc::wire::Translator;
    use std::sync::atomic::{AtomicU32, Ordering};
    static OUTER_ASKED: AtomicU32 = AtomicU32::new(0);
    fn outer_personality(_uc: &mut runtime::Uc, _: &mut Args) -> runtime::Ret {
        OUTER_ASKED.fetch_add(1, Ordering::SeqCst);
        runtime::Ret::U32(8) // _URC_CONTINUE_UNWIND
    }
    cxx();
    let class = crate::objc::runtime::class_named("NSException");
    let host = crate::objc::runtime::send(class, "exceptionWithName:reason:userInfo:", &[crate::fw::string::make("Escaping"), 0, 0]);
    let exception = super::objc::wire::test_bridge().lock().unwrap().guest_object(&Translator, host).unwrap();
    ios32::register_function("test_outer_personality", outer_personality);
    // An outer game frame's SjLj context, as the game code that called the host registered it.
    let top = {
        let _gil = runtime::hold_gil();
        runtime::engine().get_data().tls_block + 18 * 4
    };
    let outer = alloc(64);
    mem().write_u32(outer, mem().read_u32(top));
    mem().write_u32(outer + 24, runtime::stub_for("test_outer_personality"));
    mem().write_u32(top, outer);
    let (result, raised) = crate::fw::exception::catching(|| Translator.call(runtime::stub_for("objc_exception_throw"), &[exception]));
    let left = mem().read_u32(top);
    mem().write_u32(top, mem().read_u32(outer));
    {
        let _gil = runtime::hold_gil();
        runtime::engine().get_data_mut().fault = None;
    }
    assert!(result.is_ok(), "the call ends at the host: {result:?}");
    assert_eq!(raised, Some(host), "the host has the game's exception");
    assert_eq!(OUTER_ASKED.load(Ordering::SeqCst), 0, "unwinding never reached the outer game frame");
    assert_eq!(left, outer, "the outer frame's context is the top again");
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

/// GIL mode (Unicorn, the checker, ARM32_PARALLEL=0), one thread running
/// game code at a time: a thread waiting in performSelector:onThread:
/// withObject:waitUntilDone:YES, or idling in its run loop, lets go of the
/// guest lock, so the thread it waits for can run game code.
#[test]
#[ignore = "GIL mode: test-pc.sh runs it with ARM32_PARALLEL=0"]
fn gil_mode_waiting_threads_let_others_run_game_code() {
    use crate::fw::object::{define_class, methods};
    use crate::objc::runtime::{self as objc, Id, Sel};
    use std::sync::atomic::{AtomicU32, Ordering};
    static CALLS: AtomicU32 = AtomicU32::new(0);
    fn callback(_uc: &mut runtime::Uc, _: &mut Args) -> runtime::Ret {
        CALLS.fetch_add(1, Ordering::SeqCst);
        runtime::Ret::Void
    }
    /// A game method: it runs guest code, so it needs the guest lock.
    extern "C" fn game_code(_: Id, _: Sel, _: Id) {
        use super::objc::bridge::GuestSide;
        super::objc::wire::Translator.call(runtime::stub_for("test_gil_callback"), &[]).unwrap();
    }
    /// A thread serving its run loop, entered from game code (holding the
    /// guest lock) when `from_game` is set.
    extern "C" fn serve(_: Id, _: Sel, from_game: Id) {
        let _gil = (from_game != 0).then(runtime::hold_gil);
        loop {
            crate::fw::runloop::run_this_threads_loop_briefly();
        }
    }
    setup();
    assert!(!runtime::parallel(), "run with ARM32_PARALLEL=0 (test-pc.sh does)");
    ios32::register_function("test_gil_callback", callback);
    let class = define_class("GilModeTarget", "NSObject", 8);
    methods(class, &[("gameCode:", game_code as usize), ("serve:", serve as usize)]);
    let target = objc::send(objc::send(class, "alloc", &[]), "init", &[]);
    for (round, from_game) in [(1, 0), (2, 1)] {
        let thread_class = objc::class_named("NSThread");
        let worker = objc::send(objc::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, objc::sel("serve:"), from_game]);
        objc::send(worker, "start", &[]);
        // From game code (holding the guest lock), waiting until it was done.
        let (finished, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _gil = runtime::hold_gil();
            let s = objc::sel("performSelector:onThread:withObject:waitUntilDone:");
            let perform: extern "C" fn(Id, Sel, Sel, Id, Id, u8) = unsafe { std::mem::transmute(objc::imp_for(target, s).unwrap()) };
            perform(target, s, objc::sel("gameCode:"), worker, 0, 1);
            let _ = finished.send(());
        });
        let serving = if from_game != 0 { "a run loop entered from game code" } else { "a thread's run loop" };
        done.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or_else(|_| panic!("deadlock: waiting for {serving} kept the guest lock"));
        assert_eq!(CALLS.load(Ordering::SeqCst), round);
    }
}

#[test]
fn the_bridges_own_memory_comes_from_the_fast_allocator() {
    // SpellCraft on the Pixel stalled ~100 ms twice a second: about half of
    // its game thread went to GrapheneOS's hardened malloc, which every small
    // bridge allocation (each objc_msgSend asks for several) went through.
    let block = Box::new([7u8; 24]);
    assert!(unsafe { libmimalloc_sys::mi_is_in_heap_region(block.as_ptr().cast()) }, "Rust memory still comes from the system's malloc");
}

/// A game method with a variable argument list (nil-terminated extras, as
/// -[SCAlertController queueAlertWithFormatNamed:withTarget:andAction:])
/// gets every word its caller passed, in registers and on the stack, as on
/// an iPhone where objc_msgSend jumps straight into the method. Its types
/// name only the fixed arguments.
#[test]
fn game_methods_get_the_variable_arguments_their_caller_passed() {
    use super::objc::wire::{test_bridge, Translator};
    cxx();
    setup();
    // int sum(id self, SEL _cmd, int first, ...): first + r3 + two stack words.
    let code = alloc(16);
    for (i, half) in [0x9800u16, 0x9901, 0x4408, 0x4410, 0x4418, 0x4770].iter().enumerate() {
        // ldr r0, [sp]; ldr r1, [sp, #4]; add r0, r1; add r0, r2; add r0, r3; bx lr
        mem().write_bytes(code + i as u32 * 2, &half.to_le_bytes());
    }
    let (object, sum) = {
        let mut b = test_bridge().lock().unwrap();
        let host = crate::objc::runtime::send(crate::objc::runtime::send(crate::objc::runtime::class_named("NSObject"), "alloc", &[]), "init", &[]);
        (b.guest_object(&Translator, host).unwrap(), b.guest_selector(&Translator, "g32VariadicSum:"))
    };
    let class = call_stub("object_getClass", [object, 0, 0, 0], &[]).0;
    assert_eq!(call_stub("class_addMethod", [class, sum, code | 1, cstring("i12@0:4i8")], &[]).0, 1);
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let send = runtime::stub_for("objc_msgSend");
    for round in 0..2 {
        let got = runtime::call_guest(uc, send, &[object, sum, 1, 20, 300, 4000]).unwrap() as u32;
        assert_eq!(got, 4321, "round {round}: r2, r3 and both stack words reach the method");
    }
}
