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
