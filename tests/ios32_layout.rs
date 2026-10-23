//! Memory layouts and Apple system-call decoding (pure: no guest memory is made).

use arm32_bridge::ios32::{self, bind_target, decode_syscall, startup_words, syscall_return, with_carry, AppleCall, BindTarget, Provided};
use arm32_bridge::mem::{self, MemoryLayout};
use guest32_image::{Bind32, BindSource, FixupKind, LibraryRef};

#[test]
fn android_layout_is_unchanged() {
    let android = MemoryLayout::android();
    assert_eq!(android.regions, mem::MAPPED_REGIONS.to_vec());
    assert_eq!((android.stub_base, android.stub_size), (0x0001_0000, 0x0010_0000));
    android.validate().unwrap();
    assert!(android.in_stub_area(0x0001_0008) && !android.in_stub_area(0x0011_0000));
    assert!(android.executable(mem::LIB_BASE) && !android.executable(mem::MISC_BASE));
    assert!(android.has_kernel_helpers());
}

#[test]
fn program_layout_moves_stubs_out_of_a_low_program() {
    // A fixed-address program from 0x4000: code, then data.
    let program = [(0x4000, 0x0020_0000, true), (0x0020_0000, 0x002A_9000, false)];
    let layout = MemoryLayout::with_program(&program, ios32::STUB_BASE, mem::STUB_SIZE).unwrap();
    assert!(layout.in_stub_area(ios32::STUB_BASE + 8));
    assert!(!layout.in_stub_area(mem::STUB_BASE), "the Android stub area is the program's now");
    assert!(layout.executable(0x0010_2BD0) && !layout.executable(0x0020_0000));
    assert!(!layout.executable(0x2000), "the low guard is unmapped");
    assert_eq!(layout.region_end(0x0020_1000), Some(0x002A_9000));
    assert!(layout.mapped(0x001F_F000, 0x0020_1000), "a range may cross into the next region");
    assert!(!layout.mapped(0x002A_8000, 0x002A_A000), "but not past the last one");
    assert!(!layout.has_kernel_helpers());

    // A program reaching into the heap, or a stub area inside the program, is refused.
    assert!(MemoryLayout::with_program(&[(0x4000, mem::HEAP_BASE + 0x1000, true)], ios32::STUB_BASE, mem::STUB_SIZE).is_err());
    assert!(MemoryLayout::with_program(&program, 0x8000, mem::STUB_SIZE).is_err());
}

#[test]
fn apple_system_calls_use_r12_and_the_carry_flag() {
    let mut registers: [u32; 16] = std::array::from_fn(|i| 0x100 + i as u32);
    let stack = |address: u32| address ^ 0xFFFF_0000; // stands in for guest memory
    registers[12] = 4; // SYS_write
    assert_eq!(
        decode_syscall(&registers, stack),
        AppleCall::Unix { number: 4, args: [0x100, 0x101, 0x102, 0x103, 0x104, 0x105, 0x106] }
    );
    // Indirect: number in r0, arguments r1-r6, the seventh from sp + 28 (r8 is armv7k only).
    registers[12] = 0;
    registers[0] = 1;
    assert_eq!(
        decode_syscall(&registers, stack),
        AppleCall::Unix { number: 1, args: [0x101, 0x102, 0x103, 0x104, 0x105, 0x106, stack(0x10D + 28)] }
    );
    registers[12] = -31i32 as u32;
    assert_eq!(decode_syscall(&registers, stack), AppleCall::Mach(-31));
    registers[12] = 0x8000_0000;
    assert_eq!(decode_syscall(&registers, stack), AppleCall::Machine(0x103));

    // Errors: errno in r0, r1 cleared, carry set.
    assert_eq!(syscall_return(Err(9)), (9, 0, true));
    assert_eq!(syscall_return(Ok((5, 7))), (5, 7, false));
    let flags = 0xD000_0000u64; // N, Z, V
    assert_eq!(with_carry(flags, true), 0xF000_0000);
    assert_eq!(with_carry(0xF000_0000, false), flags);

    // What a program's own start finds at sp: argc, argv, envp, apple, each NULL-ended.
    assert_eq!(startup_words(&[0x10], &[], &[0x20]), [1, 0x10, 0, 0, 0x20, 0]);
}

#[test]
fn weak_imports_nothing_provides_bind_to_zero() {
    let exports = std::collections::HashMap::from([("_own".to_string(), 0x4001)]);
    let bind = |symbol: &str, source, library, weak_import| Bind32 {
        source,
        address: 0x20_0000,
        segment: 2,
        kind: FixupKind::Pointer,
        width: 4,
        library,
        symbol: symbol.into(),
        addend: 0,
        weak_import,
        lazy_offset: None,
    };
    let library = Some(LibraryRef::Dylib(1));
    let nothing = Provided::default();
    let called = Provided { called_lazily: true, ..Provided::default() };
    assert_eq!(bind_target(&exports, &bind("_gone", BindSource::Normal, library, true), false, &nothing), BindTarget::Missing);
    assert_eq!(bind_target(&exports, &bind("_gone", BindSource::Lazy, library, false), true, &called), BindTarget::Missing);
    assert_eq!(bind_target(&exports, &bind("_gone", BindSource::Lazy, library, false), false, &called), BindTarget::StandIn);
    assert_eq!(bind_target(&exports, &bind("_own", BindSource::Weak, None, false), false, &nothing), BindTarget::Program(0x4001));
    // A bind that names a library never takes the program's own copy.
    assert_eq!(bind_target(&exports, &bind("_own", BindSource::Normal, library, false), false, &called), BindTarget::StandIn);
    // A weak import something provides is bound to it.
    let function = Provided { function: true, ..Provided::default() };
    assert_eq!(bind_target(&exports, &bind("_f", BindSource::Normal, library, true), false, &function), BindTarget::StandIn);
}

#[test]
fn data_imports_get_their_own_memory_and_functions_get_stand_ins() {
    let exports = std::collections::HashMap::new();
    let pointer = |symbol: &str| Bind32 {
        source: BindSource::Normal,
        address: 0x20_0000,
        segment: 2,
        kind: FixupKind::Pointer,
        width: 4,
        library: Some(LibraryRef::Dylib(1)),
        symbol: symbol.into(),
        addend: 0,
        weak_import: false,
        lazy_offset: None,
    };
    let target = |provided: Provided| bind_target(&exports, &pointer("_x"), false, &provided);
    // Only reached through a pointer and unknown: data (zero bytes), never an 8-byte stand-in cell.
    assert_eq!(target(Provided::default()), BindTarget::UnknownData);
    // Called somewhere (a lazy bind) but unknown: a function that stops the program when called.
    assert_eq!(target(Provided { called_lazily: true, ..Provided::default() }), BindTarget::StandIn);
    // Registered data wins over the call guess; a guest library's export wins over both.
    assert_eq!(target(Provided { data: true, called_lazily: true, ..Provided::default() }), BindTarget::Data);
    assert_eq!(target(Provided { library: Some(0x0100_0101), data: true, ..Provided::default() }), BindTarget::Library(0x0100_0101));
    // A registered function reached through a pointer (a callback) is a stand-in.
    assert_eq!(target(Provided { function: true, ..Provided::default() }), BindTarget::StandIn);
    assert_eq!(ios32::c_name("__ZdlPv"), "_ZdlPv");
    assert_eq!(ios32::c_name("_OBJC_CLASS_$_NSObject"), "OBJC_CLASS_$_NSObject");
}
