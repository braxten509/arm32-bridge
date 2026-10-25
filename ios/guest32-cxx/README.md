# C++ runtime for 32-bit iPhone games (libguest32cxx.so)

32-bit iPhone games call libc++ (`std::__1` strings, streams, locale,
threads, containers' out-of-line parts), libc++abi (`operator new`/`delete`,
`__cxa_*` exceptions, guards, `__dynamic_cast`, type_info) and the SjLj
unwinder (`_Unwind_SjLj_Register/Unregister/Resume`). This folder builds
LLVM's own libc++, libc++abi and libunwind for Apple's armv7 ABI as one
32-bit ARM **ELF** library that the translator's loader (`src/elf.rs`) puts
into guest memory, so this code runs translated like the game's own.

| File | What |
|---|---|
| `fetch.sh` | fetches LLVM 19.1.7 (commit `cd708029e0b2…`), only `libcxx`, `libcxxabi`, `libunwind`, `compiler-rt/lib/builtins`, and checks each folder's git tree hash |
| `build.sh [test]` | builds `libguest32cxx.so` (+ `.imports`, `.exports`) into `~/.cache/gamebridge/guest32-cxx/out` (`GUEST32_CXX_OUT`); `test` also builds the test libraries |
| `check-library.sh` | refuses what `src/elf.rs` can't load (TLS, other relocation types, no DT_HASH, DT_NEEDED); writes the import/export lists |
| `test.sh` | the test: builds, then runs `test/cxxtest.cpp` on the translator (qemu-aarch64 on the PC) |
| `check-game-imports.sh <App.app>…` | every C++ runtime import of a game must be exported (reads the game in place) |
| `config/__config_site` | libc++ settings |
| `darwin-include/` | Game Bridge's own minimal Darwin armv7 C headers (Apple's types, sizes and constants; not Apple's files) |
| `glue/cxa_atexit.c` | `__cxa_atexit` / `__cxa_finalize` (libSystem's on iOS) |
| `glue/libstdcxx.cpp` | GCC libstdc++'s out-of-line helpers that games built with GCC's headers import: `std::_Rb_tree_increment`/`_decrement`/`_insert_and_rebalance`/`_rebalance_for_erase` (std::map/set on GCC's node layout) and `std::__throw_length_error`; Game Bridge's own code, GCC's mangled names |
| `test/` | `cxxtest.cpp` (the checks) and `testc.c` (a tiny Darwin-ABI C library standing in for the runner's C table in the test) |

The built library (1.2 MB) is not committed: it is a build output, rebuilt
exactly from the pinned sources in about a minute, and the repo keeps no
other binaries. Needs clang, llc, ld.lld, llvm-objcopy/readelf/nm, git and
network once. **Only the sources are pinned, not the tools**: the build uses
whatever LLVM is installed. Used and tested: clang/llc/lld/llvm-objcopy
23.1.1 (Arch/CachyOS packages). A different LLVM may produce different code;
rerun `test.sh` after a toolchain update. `fetch.sh` refuses a source tree
with changed, extra (untracked) or ignored files, since `build.sh` compiles
whole folders by pattern.

## How it is built

Every source is compiled by clang **as Apple armv7 code**
(`-target armv7-apple-ios5.0`: Thumb-2, `__APPLE__`, Apple's `size_t`/`ptrdiff_t`,
mangling, SjLj exceptions, Apple's struct layouts) to LLVM bitcode. `llc`
then makes ELF code from it for `thumbv7-unknown-unknown` with
`-target-abi=apcs-gnu` (Apple's argument rules and data layout),
`-mattr=+reserve-r9` and `-exception-model=sjlj`, and `ld.lld` links one
shared library. r9 is reserved because Apple's armv7 code (iPhone OS 3+)
doesn't keep r9 across calls, while this triple's rules would keep values
there across calls into game code: TownCraft's own
`basic_stringbuf::overflow` changed r9 under libc++'s `xsputn`, which then
copied from address 1. ELF names are
the C names: the Mach-O name without one leading underscore
(`__Znwm` → `_Znwm`, `___cxa_throw` → `__cxa_throw`), which is how the
runner looks them up (`ios32::c_name`).

Two things the toolchain doesn't do by itself, handled in `build.sh`:
- with APCS, LLVM's ARM ELF backend always puts static constructors in
  `.ctors` (init_array only for AAPCS); `src/elf.rs` runs `.init_array`, so
  each object's `.ctors[.N]` is renamed `.init_array[.65535-N]` (same order);
  `.dtors` fails the build (destructors go through `__cxa_atexit`);
- compiler-rt's division and 64-bit conversion helpers (`__udivsi3`,
  `__divdi3`, …; armv7 has no divide instruction) are linked in, hidden.

Left out: libc++'s `new.cpp` (libc++abi defines new/delete), C++20 waiting
atomics and barriers (Apple's private `__ulock` calls), `std::print`,
`<filesystem>`, time zones, and libc++abi's `cxa_thread_atexit`
(C++11 `thread_local` needs iPhone OS 8).

## ABI: claims and evidence

Checked against TownCraft's own armv7 code (`llvm-objdump --macho -d` of the
armv7 slice; addresses are the file's).

1. **`std::string` uses libc++'s usual layout, not the alternate one.** The
   design note assumed `_LIBCPP_ABI_ALTERNATE_STRING_LAYOUT`; TownCraft says
   otherwise. `c_str()` inlined at 0x10bd2:
   `ldrb r0,[r4]; tst.w r0,#1; ite eq; addeq r3,r4,#1; ldrne r3,[r4,#8]`
   → long flag = bit 0 of byte 0, short characters from byte 1, long data at +8.
   `size()` at 0x3b61a / 0x6961c / 0x832bc: `tst #1; ite eq; lsreq rX,rY,#1;
   ldrne rX,[s,#4]` → short size = byte 0 >> 1, long size at +4. So:
   long = {cap|1, size, data}, short = {size<<1, chars[11]}. Test checks
   0 and 1 read these bytes, and building with the alternate layout fails
   exactly those two (run_all = 0x3).
2. **`size_t` is `unsigned long`, `ptrdiff_t`/`streamsize` is `int`:** the
   game imports `__Znwm`, `…6resizeEmc`, `…7reserveEm` (m) and
   `basic_streambuf::setbufEPci`, `xsgetnEPci` (i). Clang's
   `armv7-apple-ios` gives exactly these (`__SIZE_TYPE__ long unsigned int`,
   `__PTRDIFF_TYPE__ int`).
3. **64-bit arguments take the next two registers, no even pair (APCS):**
   the call to `ostream::operator<<(double)` at 0x142bac:
   `vmov r1, r2, d16` with `this` in r0. The library's own
   `std::to_string(long long)` reads its value from r1:r2 (`cmp.w r2,#-1`
   on the high word), and test check 12 calls it by hand with r1:r2 (r3
   poisoned) and checks the text.
4. **SjLj function context = libunwind's `_Unwind_FunctionContext`:** at
   0x10ba8-0x10bbc the context is at sp+0xc: `str personality,[sp,#0x24]`
   (+0x18), `str lsda,[sp,#0x28]` (+0x1c), `str r7,[sp,#0x2c]` (jbuf[0] = r7),
   `str resume|1,[sp,#0x30]` (jbuf[1], Thumb), `str sp,[sp,#0x34]` (jbuf[2]),
   then `bl __Unwind_SjLj_Register`; the call-site index lives at +4
   (`str r0,[sp,#0x10]`) and the landing pad reads it there. libunwind's
   `__builtin_longjmp` restores r7/pc/sp from those three words (r7 is the
   frame pointer for Thumb).
5. **pthread objects are Apple's:** `std::mutex`'s constexpr constructor is
   inlined with `movw r2,#0xaba7; movt r2,#0x32aa` (0x138ae8, 0x144586,
   0x149726) = `_PTHREAD_MUTEX_SIG_init`; 0x3CB0B1BB
   (`_PTHREAD_COND_SIG_init`) appears for `condition_variable`. The headers
   here use Apple's 32-bit sizes (mutex 44 bytes, cond 28, once 8, …).
6. **ctype masks are Apple's:** `ctype_base::mask` is `uint32_t` with
   `_CTYPE_*` values and `ctype<char>`'s table is `_DefaultRuneLocale.__runetype`
   (libc++'s `__APPLE__` path). TownCraft inlines `ctype<char>::widen`
   through vtable slot 0x1c (after `__shared_count`'s three entries), as
   libc++ ABI v1 lays it out.
7. **type_info equality**: `_LIBCPP_TYPEINFO_COMPARISON_IMPLEMENTATION 2`
   (same object, else same name by `strcmp`). A superset of pointer
   equality: right when a game carries its own copy of a type_info.

## Interface with the 32-bit runner (the core helper's side)

`ios/build.rs` runs `build.sh test` when the library or its test libraries
are missing and embeds the library into the iPhone layer. Its folder is
`$GUEST32_CXX_OUT` if set, else `~/.cache/gamebridge/guest32-cxx/out-<hash>`,
the hash of every file in this folder: a change here gets a fresh build,
and checkouts with different rules never share a library. The runner
(`ios/src/guest32`), before binding the game (`ios32::load`):
1. registers its C table, then calls `elf::open_bytes(uc,
   "libguest32cxx.so", <embedded>, true)` (`true`: Apple ABI, so its imports
   use the iPhone table, not Android's): it loads the library, binds its
   imports, runs its constructors (iostream objects), and puts its exports
   into `shared().guest_exports` under C names;
2. `ios32::load` then binds each game import whose C name is in
   `guest_exports` to the library (the core branch already does this, before
   registered data and functions).

What the library needs from the core side:
- **`src/elf.rs`'s `resolve()` must ask `platform().library_import`** for
  names no loaded library defines (the hook exists in the core branch, not yet
  called from `elf.rs`). Without it, the library's imports get Android's
  handlers.
- **A guest-library region in the iPhone memory layout:** `elf::Loader` puts
  libraries at `mem::LIB_BASE` (0x01000000); `MemoryLayout::with_program`
  doesn't map that range yet. TownCraft ends at 0x2A9000 and Dungeon Plunder
  is low too, so the range is free for both.
- **Thread-specific slot 18** (Apple's `__PTK_LIBC_DYLD_Unwind_SjLj_Key`) of
  the TPIDRURO block holds each thread's SjLj context list, read and written by
  guest code (`darwin-include/unwind/System/pthread_machdep.h`). It must be
  zero when a thread starts (the runtime block is zeroed) and never used for a
  `pthread_key_create` key.
- **The C table must provide the 125 names in `libguest32cxx.so.imports`**
  with Darwin meaning. Only those actually called matter at run time; the
  test run reaches these (a game reaches at least as many): `malloc`, `calloc`, `realloc`, `free`, `posix_memalign`,
  `mem*`, `strlen`, `strcmp`, `abort`, `__error`, `pthread_once` (Apple's
  `{0x30B1BCBA, …}` once), `pthread_key_create`/`get`/`setspecific`,
  `pthread_mutex_*`/`pthread_cond_*` (Apple signatures, including
  `PTHREAD_RECURSIVE_MUTEX_INITIALIZER` 0x32AAABA2), `pthread_self`,
  `pthread_mach_thread_np`, `newlocale`/`uselocale`/`freelocale`,
  `___mb_cur_max_l`, `mbtowc_l`, data `_DefaultRuneLocale` (Apple layout:
  `__runetype` at +52, `__maplower` +1076, `__mapupper` +2100), `__stdinp`,
  `__stdoutp`, `__stderrp`. Darwin details to keep: `clock_gettime` takes
  Apple's clock ids (`CLOCK_MONOTONIC_RAW` = 4, `CLOCK_MONOTONIC` = 6);
  `snprintf_l`, `sscanf_l`, `asprintf_l`, `swprintf`, `fprintf` are variadic
  with Apple's packed 64-bit/double arguments; `long double` is `double`
  (`strtold`, `wcstold`); `isdigit_l`, `tolower` etc. are calls here (Apple
  inlines them); `dlopen("/usr/lib/libstdc++.6.dylib", RTLD_NOLOAD = 0x10)`
  must answer NULL unless a 32-bit libstdc++ is really loaded (libc++ checks
  for GCC's empty-string storage when copying exception messages).
- **`__cxa_atexit` / `__cxa_finalize`** are exported by the library (pure
  guest code). If the runner's `exit()` should run static destructors, it
  calls `__cxa_finalize(NULL)` first. Destructors run with the registration
  lock released (as Apple's Libc), so one may join a thread that registers more.

## Tests

`test.sh` (PC: needs the ARM64 cross build of arm32-bridge, `BIN=` to
point elsewhere): loads `libtestc.so`, `libguest32cxx.so` and
`libcxxtest.so` with the Android loader and calls `run_all`: 17 checks,
one bit each (string layout bytes, growth, exceptions thrown inside the
library and caught across a frame with a cleanup, catch by base class,
rethrow, `std::logic_error`, static guard, `dynamic_cast`/`typeid`,
`ostringstream`/`istringstream`, the hand-made APCS call, `__cxa_atexit`,
destructors run with the registration lock released, GCC's red-black tree
helpers on a 40-node tree, GCC's `__throw_length_error` caught as
`std::length_error`). Passes when run_all
= 0, the runner exits cleanly within the time limit, and there are 0
translator fallbacks. Without the library it stops at `_Znwm`. This is a
PC unit test (qemu-aarch64); game tests run on the Pixel only, never on
Android emulators.

`check-game-imports.sh` on TownCraft: all 114 C++ runtime imports exported;
Dungeon Plunder: all 3.

Not covered here: Objective-C exceptions (Dungeon Plunder's
`objc_exception_throw`, `__objc_personality_v0`, `objc_begin_catch`) sit on
this library's SjLj unwinder and `__cxa_*` but need the Objective-C runtime
to match classes; they belong with the Objective-C work.

Licenses: LLVM sources are Apache-2.0 with LLVM exceptions; the files here
are Game Bridge's.
