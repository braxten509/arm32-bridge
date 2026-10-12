# ARM32 → ARM64 translator (src/dynarec/)

Approved 2026-10-12. Replaces Unicorn as the engine that runs guest code;
Unicorn stays as the fallback for anything not translated yet and as the
reference for the checker.

## Why

Profiles on the Pixel 10 (simpleperf, Puzzle Craft 2) after the Unicorn
patches: about 17% of the game thread goes to Unicorn looking up indirect
jump targets (`helper_lookup_tb_ptr`, `cpu_get_tb_cpu_state`, `qht_lookup`),
several percent to soft-float VFP (`float32_add`, `float32_mul`), and every
guest memory access goes through a software TLB. Regular 65-100 ms frames
come from guest code bursts. Pure CPU work runs about 12.5x slower than
native (bench 20000: ~1370 ms on the PC vs ~145 ms native).

## Shape

- **State**: `CpuState` (`#[repr(C)]`): r0-r15, NZCV in host order, Q/GE,
  Thumb bit, d0-d31 (as 16 × 128-bit q registers), FPSCR, TPIDRURO.
- **Registers while translated code runs**:
  - guest r0-r14 → host w0-w14 (r15 is a constant at translation time)
  - x15-x17 scratch; x18 is never touched (Android shadow call stack)
  - x19 = `&CpuState`, x20 = guest memory base, x21-x28 scratch
  - guest NZCV lives in host NZCV (same meaning on both, including C for subtract)
  - guest q0-q15 → host v0-v15; v16-v31 scratch. s(2n+1) and d(2n+1) are lanes
- **Memory**: `ldr w, [x20, w_addr, uxtw]`. No checks: the 4 GiB reservation
  covers every guest address; unmapped handle ranges fault, and a SIGSEGV
  handler turns faults inside translated code into a logged guest fault.
- **Blocks**: decode until a branch (or 64 instructions), emit ARM64, cache by
  (guest pc | Thumb bit). Direct branches chain block-to-block (patched when
  the target is compiled); indirect branches (`bx lr`, `pop {pc}`, `ldr pc`)
  check an inline direct-mapped lookup table before leaving to the dispatcher.
- **Phone functions**: stubs are `svc #index; bx lr` in the stub area. A block
  that hits one returns to Rust, which runs the same handler as today and
  continues at lr.
- **Fallback**: an instruction the translator does not know ends the block;
  the dispatcher copies state into this thread's Unicorn engine, runs one
  instruction there, and copies it back. A counter per instruction pattern
  shows what to translate next.
- **Threads**: one `CpuState` per guest thread; the code cache is shared and
  only touched while holding the guest lock (one guest thread runs at a time).
- **Exclusive access**: `ldrex`/`strex` become a plain load and a store that
  always succeeds (safe because only one guest thread runs at a time).

## Checker

`ARM32_CHECK=1` (or the Android property `debug.arm32bridge.check=1`) turns the
translator on and checks translated Thumb blocks: before a block runs, Unicorn
runs the same instructions one at a time from the same registers, stopping
where the block stops (the first step that does not move forward inside the
block). A write hook records Unicorn's memory writes, which are then undone.
After the translated block runs, registers, flags, Q/GE, d0-d31, FPSCR, the
exit point and the recorded memory must match; the first difference per block
is logged with the start registers and the block's code. `1` checks each
block's first 32 runs and then every 1024th (fast enough to play); `all`
checks every run. The summary line counts checks, differences and reference
runs that could not finish in Unicorn.

## Pass checks (approved)

1. Self-test 19/19 on the new engine, PC and phone.
2. Checker: zero differences across game start-up and 60 s of play.
3. bench 20000 at most 4x native (today 12.5x).
4. Normal frames at most 3 ms of game work (glClear's wait for the screen excluded).
5. No stall over 100 ms in a level after loading.
6. 115+ fps average over 30 s in the menu and in a level, and SurfaceFlinger agrees.

Three failed fixes on one check: stop and report.

## Stages

1. CPU interface: handlers and `call_guest` use `CpuState` through one
   interface; Unicorn keeps running everything. Self-test 19/19.
2. Emitter, executable memory, dispatcher, stubs, fallback; a first Thumb
   subset (moves, add/sub/cmp, loads/stores, push/pop, branches).
3. Checker.
4. Thumb-2 and ARM coverage driven by the fallback counter.
5. VFP and NEON.
6. Block chaining and the inline indirect-jump table.
7. Phone: fault handler, threads, measurements against the pass checks.
