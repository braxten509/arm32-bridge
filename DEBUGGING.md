# Debugging: self-test "malloc + string functions" fails only with the checker on (ARM32_CHECK=all)

## Symptom
- Expected: selftest run_all 19/19 with ARM32_CHECK=all (it is 19/19 with ARM32_ENGINE=jit and with Unicorn).
- Actual: `FAIL malloc + string functions` (strlen(text)==11 && strcmp(text,"hello world")==0); the next check
  `realloc keeps contents` (strncmp 11) passes. Every time (3/3 runs).
- Setup: Pixel 10, `adb shell` CLI in /data/local/tmp/a32, selftest.so is ARM-mode code (clang default),
  so only 12 Thumb block runs get checked, all during start-up (C++ static constructors), all matching.

## Suspects (ranked)
1. A reference run leaves something changed that is not compared (Unicorn-internal state, memory not undone).
2. The write hook changes how Unicorn runs later fallbacks.

## Attempts
### 1. Hook only, no reference runs (ARM32_CHECK_HOOK_ONLY experiment)
- Predicted: fails → hook is the cause; passes → reference runs are.
- Actual: 0 failures. Conclusion: the reference runs cause it (suspect 2 ruled out, at least without TB flush).
### 2. Flush Unicorn's translated code after adding the hook (blocks translated earlier would miss write reports)
- Actual: still fails. Kept (it is still correct to flush), did not help.
### 3. Log each reference run
- Actual: 12 runs, writes only to stack (0xefffff..) and one byte at 0x1000001c; all compare equal.
### 4. Bisect: check only the k-th candidate run (ARM32_CHECK_ONLY=k)
- Actual: k=0..9 pass, k=10 fails, k=11 fails, none (99) passes.
- Run 10 = 0x0100ee31..ee3e (add.w; cmp; itt ne; ldrne; blxne — condition false), run 11 = 0x0100ee3f..ee58.
- New suspect (1a): Unicorn's IT state (CPSR bits 10-15, 25-26) is left set after a reference run, and
  write_unicorn keeps all non-NZCV CPSR bits, so a later fallback starts inside a stale IT block.
### 5. Log Unicorn CPSR after the reference run and after fallbacks
- Actual: run 10 ends with cpsr 0x600001f3 (no IT bits); no fallback ended with IT bits.
- Conclusion: suspect 1a ruled out. Run 10 writes no memory and compares equal, yet breaks a later test.
### 6. Full trace (every block entry and fallback with registers), check-only-10 vs no check; diff first divergence
- Actual: first divergence = Unicorn stops at different places (write hook changes its exit points; benign by itself).
  Then, check run only: fallback from strlen@plt (0x0101a6f8) x16 ended at 0x000109b4 with r0=0xb — that is the
  `bx lr` half of strlen's stub, right after its `svc` already ran (strlen returned 11). run() treats any stub-area
  address as "call stub (address-STUB_BASE)/8", so strlen ran a second time with r0=11 → wrong length.
- Explains all symptoms: only the first check (strlen/strcmp results) fails; memory contents are right, so
  `realloc keeps contents` (strncmp) passes; only with the hook because the hook moves Unicorn's stop points.

## Root cause
Dispatcher bug, not a checker bug: a Unicorn fallback can stop between a stub's `svc` and `bx lr`; run() then
called the phone function again. Can happen without the checker too (any fallback count that ends on the svc).

## Fix
engine.rs run(): an address at stub+4 (the `bx lr` half) just returns to LR. Temporary experiment code removed.

# Debugging: checker differences after ARM translation (2026-10-12)
- Symptom: ARM32_CHECK=all selftest: 35k checks, 4 differences. Block 0x010189e8 ends in `blx r3`;
  JIT exits to r3 (correct), Unicorn's 1-instruction step ran 6 instructions into the callee.
- Cause: unicorn uc.c uc_emu_start removes the count hook (and flushes) for count=0 runs, but re-adding it
  does not flush, so blocks translated during count=0 runs (nested guest calls from handlers inside a
  fallback, runtime.rs call_guest_with Unicorn path) never call the counter.
- Fix: flush when the count hook is re-added (vendor patch 4).
- Attempt 1 (vendor patch 4, flush when count hook is re-added): rebuilt and verified, no change. Kept (real gap).
- Evidence: differences happen at call depth 1 (not nested); all in ARM code; overrun always ends at a TB end.
- Cause: qemu/target/arm/translate.c sets uc->no_exit_request only in the Thumb translator (condexec_mask != 0);
  the ARM translator never clears it, so after an IT block, ARM TBs get no check_exit_request and count
  overruns to the TB end.
- Attempt 2: vendor patch 5 clears no_exit_request per ARM instruction.

# Debugging: game checker differences after IT blocks that return (2026-10-12)
- Symptom: game, ARM32_CHECK=1, 75 s: ~986k checks, 30 differences. All shown: Thumb blocks ending in an IT block
  whose last instruction returns (bx lr / pop {pc}); JIT result correct, Unicorn ran 1+ caller instructions too
  (case 3: the extra caller instruction ran as if conditional).
- Suspect: write_unicorn keeps Unicorn's CPSR IT bits; stale IT state makes Unicorn translate the next code as
  inside an IT block (no exit checks, conditional execution).
- Attempt 1: clear IT bits in write_unicorn and count how often they were set (STALE_IT in [jit] stats line).
- Result: "0 starts with leftover IT state", same 30 differences. Ruled out (fix kept: harmless, correct).
- Attempt 2 (evidence, not a fix): code hook records every instruction Unicorn runs per step; printed in reports.
- Evidence (attempt 2): Unicorn ran (step 2) itt + IT body ending in pop {pc}/bx lr, then 2-5 caller instructions
  whose code hooks fired (so the count hook asked to stop) before it actually stopped. Stop requests right after
  a branch out of an IT block are lost for a few instructions (exact QEMU cause not found).
- Attempt 3 (checker only): the checker's own code hook calls emu_stop before any instruction outside the block.
- Result attempt 3: no change; Unicorn ran caller instructions even after emu_stop from our own hook. Reverted.
- Status: 3 attempts on this check, stopped and reported to the user (2026-10-12). In all 30 cases the JIT
  result is the correct one; Unicorn (the reference) runs past an IT block that ends in a branch.
- Decision (user, 2026-10-12): label these runs. A reference run where Unicorn ran any instruction outside the
  block is counted as "not compared (Unicorn ran past the block)" instead of a difference.
- Result (2026-10-12): game, ARM32_CHECK=1, 75 s: 786,432 checks, 0 differences, 22 not compared (ran past the block).
  Self-test ARM32_CHECK=all: 35,363 checks, 0 differences, 0 ran past. Pass check 2 met.

# Stage 5 progress (2026-10-12)
- VFP translated (src/dynarec/vfp.rs), FPSCR control/flags moved through enter/exit glue, LDM IB/DA, Thumb BX PC,
  ARM register-shifted MOV/MVN fix. Game with ARM32_CHECK=1: 1,077,554 checks, 0 differences, 30 ran past;
  55-75 fps under the checker (was ~3). Fallback sites 5345 -> 75; the rest are Thumb NEON (by-scalar float multiply,
  VLD1/VST1).
- Without checker (engine=jit): 91-108 fps, game code 9-10 ms/frame; ~600k fallbacks/s, all Thumb NEON matrix code.
- Sub-agent review (Sol 6.1 high): VMSR kept exception-enable bits (fixed: mask 0xFFF7009F); FPSCR LEN/STRIDE not
  checked (fixed: dispatcher sends blocks to Unicorn while LEN/STRIDE != 0, and VMSR ends the block).
- NEON (neon.rs): VLD1/VST1 1-4 regs and 32-bit lane, 3-same float/int/logic, VMUL/VMLA/VMLS.F32 by scalar; host FPCR
  switched to the ARMv7 standard FPSCR value around NEON float. Game with checker: 1,310,720 checks, 0 differences,
  90-107 fps under the checker; fewer than 4M fallbacks in 75 s (no stats print).

# Stage 6 start: own code per frame (2026-10-12)
- Frame timer now splits own code / phone functions. Menu, engine=jit: 110-122 fps (median ~117); own code 3.9-5.1 ms
  per frame (goal <=3 ms); phone functions 2.9-4.2 ms (glClear screen wait ~5 ms on most frames, excluded).
- Spikes: own code 45-59 ms every few seconds (unknown yet); glDrawElements 16-31 ms inside the GPU driver.
- Next: simpleperf profile of the game process before choosing what to change.
- Sub-agent review (Gemini 3.8 high): enter glue compared masked guest control bits with the unmasked host FPCR
  (would rewrite FPCR every block if the host had other bits set). Fixed: mask both. Everything else confirmed.
- Profile (simpleperf, 8 s, menu): engine::run 34%, call_stub 12%, SipHash block lookup 7%, malloc/free 4%,
  translated code incl. glue ~20%. Dispatcher overhead dominates -> stage 6 (block links + jump table).
- Stage 6 (block links + 4096-entry jump table + cheap pc hash): self-test 0 failures (all engines); bench 141-144 ms
  vs native 106-147 ms (~1.0-1.3x); menu 119.8-120.1 fps, own code 2.2-2.4 ms/frame, own-code spikes up to 8.4 ms,
  slowest frame 25.6 ms (phone functions).
- Stage 7a: fault handler for translated code (logs guest address, block, r0-r14, then chains to Android's handler).
  Found: the host reservation was RW everywhere, so a NULL read in translated code returned 0 where Unicorn faults.
  Fixed: gaps outside MAPPED_REGIONS are PROT_NONE. Throwaway /tmp fault.so: Unicorn "READ_UNMAPPED at 0x10",
  translator "[fault] ... at 0x00000010 ... guest block 0x01000328", then SIGSEGV. Self-test, bench, game (jit and
  checker: 1,048,576 checks, 0 differences) all still pass.

# Stage 7b: level measurements (2026-10-12)
- Level (idle on the board, no moves), engine=jit, 31 s: game counter min 95.2 / median 119.9 / max 122.5 fps;
  own code avg 1.08 ms/frame; SurfaceFlinger timestats 3729 frames, 0 dropped, 0 janky. One 148 ms frame (own 119 ms)
  in the first measured second, ~7 s after tapping Continue.
- Level entry: one ~1 s frame (own 856 ms). simpleperf of the render thread over entry: translated code 39%, bridge
  .so 16% (call_stub 10.5% self, incl. inlined handlers), kernel 16%, libc 13% (allocations are in gralloc/EGL during
  eglSwapBuffers/glClear, not the bridge), translate() 0.03%, Unicorn ~0. So: the game's own level-loading work, not
  translation or fallbacks.
- Play test (user approved moves), engine=jit, 8 matches over ~106 s: every second >=115 fps (median 120), slowest
  play frame 32 ms; opening the level 74 ms; SurfaceFlinger timestats 15,573 frames, 0 dropped, 0 janky. Repeating
  22-32 ms frame (own code) about every 5 s. The ~1 s freeze is the first load of the village map after Continue on
  the title screen (999 ms, own 838 ms), not opening a level.
- Check 2 in play: checker=1, start-up + resumed level + 4 matches: 2,097,152 checks, 0 differences, 50 ran past.
  User saw stutters during this checker run (checker replays sampled blocks in Unicorn). Translator restarted for
  the user to judge without the checker.
- User: smooth without the checker; loading freeze on first village load does not count. Translator is now the
  default (ARM32_ENGINE / debug.arm32bridge.engine=unicorn opts out). Self-test passes on all engines; game start-up
  124 ms with no property set (Unicorn: 2722 ms).
