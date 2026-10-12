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
