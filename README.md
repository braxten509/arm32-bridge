# arm32-bridge

Proof of concept: run 32-bit ARM Android game code on the 64-bit-only Pixel 10.

The Pixel 10's CPU has no 32-bit mode, so 32-bit-only APKs (like Puzzle Craft 2) won't install. This program
loads a game's 32-bit native library into a 64-bit process. It runs the library's instructions with the
[Unicorn](https://www.unicorn-engine.org/) ARM32 engine and answers the library's calls to the phone (libc,
libm, liblog, libdl, JNI) with 64-bit host code.

## How it works

- `src/mem.rs`: the whole 32-bit address space is one 4 GiB host reservation, so guest address `g` is host
  address `base + g`, and Unicorn shares the same pages. The guest's malloc is dlmalloc running over the guest
  heap region.
- `src/elf.rs`: loads ELF32 ARM shared libraries: copies segments, applies `R_ARM_RELATIVE/ABS32/GLOB_DAT/
  JUMP_SLOT` relocations, runs `DT_INIT` and `DT_INIT_ARRAY`.
- `src/runtime.rs`: each imported phone function gets an 8-byte stand-in (`svc #n; bx lr`). The `svc` traps into
  Rust, which reads arguments the ARM32 softfp way (r0-r3, then the stack; 64-bit values in even register pairs),
  runs the handler and writes r0/r1. `call_guest` calls guest code from Rust and can be nested, which qsort,
  pthread_once and the start-up code need.
- `src/imports/`: the handlers. They convert sizes and struct layouts where 32-bit and 64-bit differ (`long`,
  `time_t`, `struct stat`, `struct tm`, `open()` flags, va_list). `jni.rs` is a pretend Java side that logs every
  class lookup and Java call.

## Build and run

```sh
cargo build --release                 # PC (x86_64) build, for quick testing
./build-phone.sh                      # phone (arm64 Android) build, needs NDK 30
tests/guest/build.sh                  # 32-bit test libraries + native speed test

./target/release/arm32-bridge call tests/guest/build/selftest.so run_all
./target/release/arm32-bridge load path/to/libcocos2dcpp.so --jni-onload
./target/release/arm32-bridge bench tests/guest/build/bench.so bench 20000
```

Flags: `--trace` prints every phone call, `--verbose` prints each start-up function.

## Results (2026-10-12, Pixel 10, Termux)

- Self-test (`tests/guest/selftest.cpp`): 15/15 checks pass on the PC and on the Pixel. Covers a C++ static
  constructor, strings, printf sizes, double/float math, mixed argument passing, qsort calling back into 32-bit
  code, pthread_once, thread-specific data, setjmp/longjmp, a C++ exception caught through the unwinder, file
  reading, and strtol's 32-bit clamping.
- Puzzle Craft 2 (`libcocos2dcpp.so`, 15 MB, ARMv7 Thumb-2): all 154 start-up functions run and `JNI_OnLoad`
  returns `0x10004` (JNI 1.4). It used 19 phone functions, all handled, in 0.38 s.
- Speed: `bench(20000)` takes about 1,830 ms through the bridge vs about 145 ms as native 64-bit, so the bridge is
  about 12.5x slower for pure CPU work. Both builds give the same result.

## Not done yet

Drawing (GLES/EGL), sound (OpenSL ES), touch, assets (AAssetManager), real threads (`pthread_create` returns
EAGAIN, and mutexes are no-ops with one guest thread) and running inside an Android app (the Java side is
pretend). Phone functions with no handler return 0 and are listed in the report.

## License

Copyright (C) 2026 Braxten Chenay

This program is free software; you can redistribute it and/or modify it under the terms of the GNU General
Public License as published by the Free Software Foundation; either version 2 of the License, or (at your
option) any later version. See [LICENSE](LICENSE).

The bridge and the converter are built with [Unicorn](https://www.unicorn-engine.org/) (GPL version 2, patched
copy in `vendor/unicorn-engine-sys`, changes listed in its ARM32-BRIDGE.md), so those programs as built are
shared under GPL version 2.
