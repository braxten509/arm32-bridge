# Resume checkpoint — Card Wars, 2026-10-12

Read this section first. The older notes below are history, not the current blocker.

## Goal and limits
- Project: `/home/braxtenchenay/dev/arm32-bridge`. Make 32-bit Android games work on the
  64-bit Pixel 10 through the shared bridge and Game Bridge converter app.
- Active game: Card Wars 1.12.9, package `com.ThugBoatLTD.CardWars`, Unity 2022.3.33f1,
  Mono, original APK: `~/dev/arm32-bridge-games/cardwars/CardWars.apk`.
- User said to hold off on the phone screen and use an emulator. Continue on a headless
  emulator only; do not launch games, tap, or capture the screen on the phone.
- Keep all existing work. Many source files have uncommitted changes. No destructive Git
  commands. The user's latest request only authorized adding this checkpoint, not more fixes.
- Inside Crush, use shared skills/memory and chat instructions; do not load project or
  personal instruction files. Run tests/builds through `crush bg` with a timeout. Keep
  builds on CPU cores `6-11,18-23` via systemd-run. No visible windows.

## What already works
- Puzzle Craft 1/2 previously reached their title/menu at about 120 fps with saves kept.
- Shared loader really opens guest libraries via dlopen; dlsym searches the selected
  library. Each original game library has a shared stand-in; bridge ships once.
- Added memory mapping, futex waits, ARM cacheflush invalidation for Mono-generated code,
  signals for Mono's thread pause/resume, EGL/window support, GLES 3.x, and libc functions.
- Added eight EGL timing/sync extension functions after Unity jumped through a null pointer.
- Undefined weak imports now resolve to zero. This did not fix the later crash.
- A real ARM carry-flag bug was fixed: register shifts now use T6 for their result instead
  of overwriting the shift amount in T2. Six regression cases pass on PC (Unicorn) and
  phone (fast translator); both self-test runs reported zero failures. This fix did NOT
  fix Card Wars. Do not redo it or call it the crash's cause.

## Remaining phone crash (saved evidence; do not run phone tests now)
- Unity reaches `Product Name: Card Wars`, then faults at guest address `0x000002c0`,
  block `0x02084505` (libunity offset `0x107f505`). r6 is zero during a copy; saved
  registers repeat across runs: r4=0x248, r5=0, r6=0, lr=0x020dcde1.
- Signal logging showed registers unchanged across delivery; that theory is ruled out
  by the tests performed. The translator remains a suspect, not a proven cause.
- Sampled checker: 51 differences in two blocks. One was the fixed carry bug; the other
  was STREX with no matching Unicorn exclusive monitor (checker limitation). It did not
  isolate the crash. Initial Unicorn/checker runs were invalid because the phone shade
  paused the game. There is no valid Unicorn-only comparison past the crash yet.
- The requested all-block checker run was not completed. Do not assume it happened.

## Emulator work and exact stopping point
- AVD `pocket_test`, Android 34 x86_64; previously `emulator-5554`. Start with `-no-window
  -no-audio -no-snapshot -gpu swiftshader_indirect -memory 4096`. Its tracked service
  job `0A2` was canceled; check whether the emulator still exists before using it.
- ARM64 bridge CLI failed under Google's ARM translation layer, so native x86_64 Android
  support was added: `.cargo/config.toml`, `build-phone.sh` TARGET option, converter
  `--abi`, `tools/convert.sh --abi x86_64 --device <serial>`, x86_64 JNI trampolines in
  `src/natives.rs`, and x86_64 zero-return stub in `tools/standin/standin.c`.
- Native x86_64 bridge self-test on the emulator: zero failures. Stand-in compiled for
  both ARM64 and x86_64. Emulator uses Unicorn; it cannot test the ARM64 fast translator.
- Latest emulator game run (`0A5`): process alive, libmain-arm32 JNI_OnLoad returned
  `0x10006`, then `NativeLoader.load failure, Unity libraries were not loaded` / `Your
  hardware does not support this application`. No libunity load appeared in that log.
  Being alive is NOT a title-screen pass. This is the current emulator blocker.
- Last code edit, not yet built/tested: `src/app.rs` enables the existing function-call
  trace when `debug.arm32bridge.trace=1`; `src/runtime.rs` sends trace lines to Android
  logging. Purpose: find why NativeLoader.load returns false on x86_64.

## Next step and checks
1. Inspect the existing diff and this checkpoint; do not rebuild the finished work from
   scratch. Restore only the headless emulator service if needed.
2. Build the current x86_64 code through a tracked background job. Pass TARGET through
   systemd-run with `-E TARGET=x86_64-linux-android`; setting it outside systemd-run did
   not reach the build. Require the real command's exit status and fresh output files.
3. On the emulator's explicit serial, set `debug.arm32bridge.trace=1`, convert/install,
   run Card Wars, save the full log, then set trace back to `0`. Read the NativeLoader
   call and its result before choosing a fix. Authorize any new edit plan as required.
4. Behavioral check: log must show libunity and Mono loading beyond the current failure;
   final game check is a visible title screen in an emulator screenshot, not just a
   live process. Recheck the bridge self-test after changing the failing part.
- Scratch runner: `/tmp/a32k/cw-emu.sh [seconds] [--no-convert]`; outputs
  `/tmp/a32k/cwe.log`, `/tmp/a32k/cwe.png`, `/tmp/a32k/CardWars-x86_64.apk`.
  Phone history: `/tmp/a32k/cw.log`, `diffs.txt`, `stacks.txt`, `cwlib/`.
  Scratch files may disappear. Runner currently masks some failures with pipelines:
  exit code zero alone is not trustworthy; inspect raw conversion/build/run results.
- Do not widen scope or redo passed parts. If the same part fails after three fixes,
  stop and ask before changing the plan. Use the kept deep-debugging skill for resumed
  debugging; its fresh-review step is extra evidence, not a behavioral test.

## Emulator attempts after the checkpoint
### E1. Trace of NativeLoader.load failure (debug.arm32bridge.trace=1, run 2026-10-12 20:51)
- Build fresh (libarm32_bridge.so 20:51:00, APK 20:51:21, convert exit 0).
- Trace: libmain's NativeLoader.load runs Android's cpufeatures: reads /proc/cpuinfo into a 0xd00
  buffer, memmem for "CPU architecture", "Features", ...; getauxval(AT_HWCAP/HWCAP2) returns 0
  (bridge design). The x86 /proc/cpuinfo has none of the ARM fields -> no ARMv7/NEON -> "Your
  hardware does not support this application". Never reaches dlopen(libunity).
- Phone passes because the arm64 /proc/cpuinfo has "CPU architecture: 8". Emulator-only blocker.
- Fix (x86_64 host only, phone build unchanged): open/__open_2/fopen of /proc/cpuinfo give the
  guest a memfd with a 32-bit Cortex-A15 view (Unicorn's model: neon vfpv4 idiva, arch 7).
- Check: emulator log shows libunity-arm32 loaded and "game code opened ... libunity.so".
- Result: passed. Emulator went much further than the phone ever did: libunity, Mono, scene
  load ("OnLevelWasLoaded was found on SLOTGame"), then SIGFPE in Kochava..cctor (E3).
### E2. Phone, Unicorn only (user allowed phone use again 2026-10-12), engine=unicorn
- Same fault as the translator: READ_UNMAPPED 0x2c0 at pc 0x020845ba (libunity 0x107f5ba),
  lr 0x020dcde1. => The translator is NOT the cause (suspect ruled out).
- Static reading of libunity 0x107f4c4: r5 = first argument (a 0x2c4-byte context) is NULL;
  r6 = 0 only because it was reloaded from [r6+0xa0]. The earlier "r6 lost across a call"
  reading was wrong. The context is created by 0x10876b4; one of its init steps failed, the
  cleanup at 0x1087a62 set it to NULL, and the caller (0x108b19e) uses it anyway.
- 0x10d836c (an init step) checks android_getCpuFeatures() (0xfe635c) bit 2 NEON / bit 1
  VFPv3, else returns error 0x30. On the phone, cpufeatures sees getauxval = 0, the 64-bit
  /proc/self/auxv, and the arm64 /proc/cpuinfo ("fp asimd aes pmull sha1 sha2 crc32", none
  of the 32-bit names) -> no NEON/VFPv3 (but ARMv8 crypto) -> init fails -> crash.
  Emulator with the E1 cpuinfo had "neon vfpv3" -> passed the same point. Explains all
  symptoms: same on both engines, phone-only, deterministic registers.
- Also seen: under Unicorn, Unity worker threads stop with INSN_INVALID right after a Thumb
  YIELD (0x440484). Translator treats YIELD as a no-op. Unicorn-only side issue; not fixed.
### E3. Emulator SIGFPE in System.DateTime..ctor (Kochava..cctor)
- New raise() log: stack shows caller libmonobdwgc 0x12e97c: Mono's magic-number division
  helper computes t % abs(d); abs had no handler and returned 0 -> __aeabi_uidivmod by 0 ->
  __aeabi_idiv0 -> raise(SIGFPE).
### E4. Fix for E2 + E3 (one change, all hosts)
- abs/labs/llabs/imaxabs handlers.
- /proc/cpuinfo on every host = 32-bit Cortex-A15 view (no crypto names); /proc/self/auxv =
  empty 32-bit table (agrees with getauxval = 0, readers fall back to cpuinfo).
- Self-test cases added (abs family; cpuinfo via fopen and open).
- Checks: PC self-test 0 failures; emulator past Kochava; phone (translator) past the 0x2c0
  crash; Puzzle Craft 2 still ~120 fps on the phone (it also reads cpuinfo via cocos2d-x).
- Results so far: PC self-test 0 failures (new cases fail without the fix: x86 cpuinfo, abs 0);
  bench 1408 ms. Emulator: no SIGFPE, Unity renders (47-54 fps reported for ~2 s).
### E5. Unicorn YIELD/WFE (vendor patch 6) -> emulator worker threads no longer stop
  (INSN_INVALID count 19 -> 0). PC self-test 0 failures after the patch.
### E6. Emulator: black screen, every thread stops ~2 s after the first frames
- debuggerd -b: all guest threads wait for the guest lock (Mutex::lock_contended) except
  "Thread-3", sitting in host poll() inside the poll handler, which kept the lock.
- Same thread just got socket() = 0 ("no handler yet for socket") and inet_pton = 0, so it
  polls fd 0 in a loop (10 ms timeouts), re-taking the lock each time -> everyone starves.
- Fix: poll releases the guest lock while waiting; new sockets.rs (socket, socketpair,
  connect, bind, listen, accept(4), shutdown, getsockname/peername, send(to), recv(from),
  set/getsockopt with timeval conversion, select, inet_pton/ntop/addr, getaddrinfo /
  freeaddrinfo / gai_strerror); blocking calls run without the lock. Self-test cases added.
- Result: emulator shows the Card Wars TITLE SCREEN (Finn + Jake, logo) at ~13 s; Unity
  loads language files. Upsight/Adobe ClassNotFound exceptions are the game's own (stripped
  SDK classes), harmless. Then frames stop: UnityMain 100% CPU in guest code, no frames.
- Same moment: "no handler yet for lseek64; returning 0".
### E7. File functions with 64-bit offsets
- Added lseek64, pread(64), pwrite(64), ftruncate(64), truncate, fsync, rmdir, strnlen,
  localtime_r. Self-test case (lseek64 / pread64 / localtime_r) passes on PC.
- Result: same freeze after the title (lseek64 was not the cause).
### E8. Freeze after the title: a spinning thread never lets go of the guest lock
- debuggerd -b x3: UnityMain inside Unicorn-translated code each time, 96-100% CPU, every
  other guest thread waiting for the lock (31 lock_contended). No time slicing exists, and
  YIELD had become a no-op (E5), so a spin-wait never hands the lock to the thread it waits on.
- Fix: YIELD/WFE hand the guest lock over (share_gil: release, yield until another thread
  has taken it or 100 µs pass, retake, deliver pending signals with the YIELD as the resume
  point). Translator: Thumb BF10/BF20 and ARM E320F001/2 end the block with exit::YIELD.
  Unicorn (vendor patch 6 revised): helper_yield calls the interrupt hook with EXCP_YIELD.
- Self-test "a thread spinning on YIELD lets the thread it waits for run" (ARM + Thumb):
  old binary FAIL, new binary ok; 0 failures; bench 1355-1362 ms (unchanged).
- Result: emulator still stops at WAITING_FOR_USERID; UnityMain 100% in guest code.
### E9. Phone (user allowed ~10 min, 2026-10-12 21:33), translator, all fixes so far
- The old libunity 0x2c0 crash is GONE: the phone shows the Card Wars TITLE SCREEN
  (screenshot /tmp/a32k/cw-small.jpg). E2/E4 confirmed on the real target.
- Same stall as the emulator after "WAITING_FOR_USERID": [fps] 0.1 fps, 12.5 s/frame of which
  10 s inside phone functions; UnityMain 99.5% CPU, 37 threads waiting for the guest lock.
### E10. Traced phone run: Mono's Boehm GC runs ~15 times a second, each pause 20-50 ms
- t2 (UnityMain) loops: pthread_kill(SIGPWR) to 3 threads, sem_timedwait for acks, then
  pthread_kill(SIGXCPU) restart + acks; 76k pthread_getspecific (Mono allocation path).
- Every signal is delivered (37 of each per thread), but one thread is often late: t9 sat in
  nanosleep (31.8 ms, guest lock released) and only answered when the sleep ended; real
  Linux interrupts the sleep. Others answered 5-7 ms late (every wake_guests wakes ~37
  threads that all queue for the guest lock).
- Fix 1 (this attempt): nanosleep / usleep / sleep / new clock_nanosleep sleep in 1 ms
  slices without the lock and come back early when a signal is pending, so the handler runs
  at once, then sleep the rest. Self-test "a signal reaches a thread inside usleep at once".
- Not yet: the thundering herd on the shared condition variable; why GC runs so often.
- Result: PC self-test new case fails on the old binary, passes on the new; emulator still
  stalls at WAITING_FOR_USERID (fix 1 alone does not unstick it).
### Sub-agent review (codex gpt-6-astra, high), 2026-10-12 21:45
- Challenged: "15 pauses/s" is not proven to be 15 full collections, and GC cost alone does
  not explain no progress. Ranked next: (1) read GC_get_gc_no / heap size; (2) a game-side
  retry/allocation loop (missing callback, JNI returning 0, unknown imports returning 0) —
  sample guest PC/LR; (3) signal context lives in a bridge heap block, not on the guest
  stack, so registers only in the context may be invisible to Boehm; (4) /proc/self/maps and
  stat expose host addresses, partial munmap is a no-op, pthread_getattr_np for other threads
  returns base 0. Our response: added guest caller sampling (debug.arm32bridge.sample=1:
  every 0.5 s per thread, lr + library return addresses + managed frames via mono_pmip).
### E11. Sampling the stalled main thread (emulator)
- Native chain is stable: mono_runtime_invoke -> managed code -> mono_threads_enter_gc_safe_region
  / mono_tls_get_lmf_addr -> pthread_getspecific, i.e. managed code looping over icalls.
- Managed frames (mono_pmip, resolved only once debug.arm32bridge.sample=2 is set after start;
  calling it earlier crashes Mono): TFUtils.SetupDebugChannels -> GetJsonFileContent ->
  GetAndroidFileContents, which is `while (!www.isDone) {}` on a jar:file:// UnityWebRequest
  (decompiled Assembly-CSharp, TFUtils.cs:588). WAITING_FOR_USERID is only SessionManager.Start.
- ROOT CAUSE: the main thread busy-waits through icalls (phone functions) without YIELD and
  never gives up the guest lock, so the worker that would finish the request almost never
  runs (only during GC pauses -> 12 s frames). On a phone both run in parallel.
### E12. Fix: time slicing of the guest lock
- Every lock acquisition goes through take_gil, which counts threads blocked on the lock
  (WANTING). wait_for_guests now waits on its own event counter + mutex (no lost wake-ups)
  and then takes the lock through take_gil, so woken threads are counted too.
- call_stub: if this thread has held the lock >= 2 ms and someone wants it, hand it over
  (share_gil; signals then resume at lr). share_gil (also used by YIELD) does nothing when
  nobody waits.
- Self-test "a thread looping over phone functions lets the thread it waits for run".
- First try delivered signals during the time-slice handoff (before the phone function ran):
  self-test hung in the pause/resume test (restart signal consumed before sigsuspend began).
  Time-slice handoff no longer delivers signals (they still run after the phone function).
- Result: PC self-test 0 failures (new case FAIL on old binary); bench 1352-1355 ms (same).
  Emulator: frames keep coming (~27 fps under Unicorn), debug channels activate, and the
  game shows its interactive "Register/Login" dialog (/tmp/a32k/cwe-small.jpg). STALL FIXED
  on the emulator. Phone check (translator) + Puzzle Craft 2 regression check still to do.
- Note: earlier PC benches of 1665 / 2782 ms came from accidentally running the same job
  twice at once (crush bg + a second background copy); not a regression.
### E13. Phone (user allowed 10 + 15 more minutes): past the hang, but loading is very slow
- Translator run reaches the same point but loads the language files at 1-3 s each
  (emulator under Unicorn: ~0.5 s). simpleperf: Thread-6 mostly in Unicorn fallback
  (uc_reg_read/write, cpu_exec). Fallback stats (debug.arm32bridge.stats=1, now printed
  every 10 s): 2.46M fallbacks, nearly all Thumb UMAAL (fbe3 1460, libunity big-number
  math), plus ARM SMMUL (e751f211, Mono) and NEON VMOV immediate.
- Added UMAAL (ARM + Thumb) and SMMUL/SMMLA (+R) to the translator; self-test case for both
  instruction sets. PC 0 failures; phone translator 0 failures; phone ARM32_CHECK=all
  self-test 41087 blocks, 0 differences; phone bench 197.6 ms. Fallbacks 2.46M -> 98K.
- Still slow. New [fps] field "waiting for the guest lock": UnityMain waits 1.0-3.7 s of
  each 1.6-6.7 s frame for the lock. Every wake_guests() woke all ~37 waiting threads,
  which all queued for the lock, and time slicing (WANTING > 0) handed it away constantly.
### E14. Fix: targeted wake-ups
- Each thread has its own parker; waits register (thread, key). Keys: guest address of the
  mutex / condition / semaphore / futex word / rwlock / once control, join = 1<<32|handle,
  sigsuspend = 2<<32|handle. wake_on(key) wakes only those waiters; a signal wakes only its
  target thread (wake_thread). wake_guests() still wakes everyone (now unused).
- Result (PC 0 failures, bench 1385-1397 ms; phone self-test 0 failures; phone run 50 s,
  /tmp/a32k/phone7.log): lock wait per frame 1.0-3.7 s -> 45-177 ms. Language files load
  in 0.1-0.6 s each (was 1-3 s). The game got further than ever: "Login: SocialID=",
  "Starting to User login", server download done, manifest fetched from
  cardwars.retroretreat.net, state PATCHING -> LOAD_DATA, db_Characters / db_Decks loaded,
  main menu buttons object created. Frames still ~8.6 fps (115 ms, own code 90 ms, lock
  wait ~50 ms) during LOAD_DATA. Card Wars force-stopped afterwards.
### E15. Emulator re-check (keyed waits): further, then Mono abort + host heap crash
- /tmp/a32k/cwe.log: login, PATCHING -> LOAD_DATA, db_Decks loaded, main menu buttons
  (same as phone). Then thread 0x10a0: Mono "* Assertion: should not be reached at
  threads.c:1350" -> abort(); 13 s later SIGSEGV in the guest heap's dlmalloc (called from
  gles map_into_copy) = guest heap metadata corrupted.
- Cause: pthread_exit was not implemented (it is in the missing-functions list), so it
  returned. Mono's start_wrapper calls mono_thread_info_exit -> pthread_exit, then
  g_assert_not_reached(). The thread then ran its TLS destructors after Mono's own exit
  path had already torn its thread info down: a likely double free, which fits the later
  heap corruption.
- Fix: pthread_exit stores the value and stops the guest (same unwinding as abort through
  nested calls); run_thread clears the stop, runs key destructors and hands the value to
  join. Self-test: thread exits from inside a qsort callback; join gets 77, code after the
  call never runs, destructor runs.
- Check after fix: phone (translator) self-test 0 failures incl. the new case; Card Wars on
  phone 100 s: no Mono assertion, no crash; login -> LOAD_DATA -> db_Decks -> main menu
  buttons; then states "WAITING_FOR_USERID" again and frames drop to 1.0-1.6 fps (phone
  functions 300-460 ms, lock wait 400-600 ms per frame). Emulator: no assertion either, but
  the same dlmalloc SIGSEGV from map_into_copy on the render thread ~50 s in. So the heap
  damage is a separate bug. Puzzle Craft 2 on phone (regression check): 117-120 fps, own
  code 2.2 ms, lock wait 0, save files kept.
- GC: Boehm suspend/restart signals (24/30) arrive ~400 per 10 s during loading (many
  collections), almost none in the 1 fps phase, so that phase is not GC.
### E16. Heap guard (evidence for the heap damage)
- free/realloc now check dlmalloc's chunk header first (in heap range, aligned, in-use bit,
  sane size, next block's prev-in-use bit) and refuse + log "[heap] refused ..." with the
  caller lr (first 30). Self-test: double free + free of a stack address leave the heap
  intact (both refusals logged). PC 0 failures, bench 1357 ms.
- Emulator run (emu16.log): no refusals at all, same SIGSEGV in dlmalloc from
  map_into_copy. So nobody frees a bad pointer; the damage comes from writes.
### E17. Root cause of the heap damage: mapped GL buffers were keyed by target
- gles.rs kept mapped-buffer copies per *target*. GL keeps buffer A mapped when buffer B is
  bound to the same target and mapped too. The old code replaced A's record with B's, and
  unmapping A later uploaded and freed B's copy while the game still wrote into it: a
  use-after-free in the guest heap, hit next in malloc (map_into_copy) on the render thread.
- Fix: key by buffer name (asked from GL with glGetIntegerv of the target's binding,
  because glBindBufferBase/Range aren't tracked); glDeleteBuffers drops and frees a mapped
  buffer's copy; a log line reports mapping while another buffer on the target is mapped.
- Emulator (emu17.log, 150 s): the log fires on target 0x8f37 (GL_COPY_WRITE_BUFFER,
  Unity's staging copies), buffers 32/34 alternating, confirming the old code freed the
  wrong copy. No crash. The game goes LOAD_DATA -> CHECK_SAVE_CONFLICT -> LOADING ->
  MESSAGE_FETCH -> SAVING -> READY and shows the tree fort with a Terms of Use dialog
  (/tmp/a32k/cwe17-small.jpg), ~9.5 fps under Unicorn.
### E18. Phone with the fix (user allowed 15 more minutes, ~22:55)
- Self-test 0 failures (both new cases). Card Wars 150 s: no crash, renders the tree fort
  with Jake and Finn and "connecting..." (/tmp/a32k/cw9-small.jpg), but only 0.5-1.5 fps
  (lock wait 600-700 ms, phone functions 450-520 ms per frame) and never reaches READY,
  while the emulator under Unicorn reached READY at ~9.5 fps. So the phone is ~10x slower
  than the PC emulator here: something phone/translator specific.
- logcat lost the start: 21K bridge lines in 105 s, mostly "[slow]" for waiting calls
  (syscall futex 10K, usleep 8K, postFrameCallback 2K). "phone functions" time includes
  those waits.
- Next: simpleperf + per-thread CPU over 15 s in the slow phase, translator stats with a
  new "dropped after code changes" counter (Mono JIT writes code at run time).
### E19. Phone profile of the slow phase: Unicorn fallbacks, not locking
- simpleperf 15 s (/tmp/a32k/perf3-*.txt): the busiest thread (arm32-guest-108, 34%) and
  UnityMain (18%) spend their time in the bridge's fallback path (reg_read_arm,
  uc_reg_read/write, engine::read/write, cpu_exec, sigsetjmp), not in translated code.
- Translator stats: ~150K fallbacks per second (4.49M -> 6.40M -> 7.90M every 10 s);
  217K blocks, 19.7K dropped after code changes (small).
- Top sites: one libunity loop at 0x020c91f2-0x020c9296 (file 0x10c41f0: int16 -> float
  audio conversion with resampling), 13 sites x 384K: VMOV.16 core->scalar, VLD1.16 one
  lane, VMOVL.S16/U16, VCVT.F32.S32. Then ARM `push {r0,r1,r12,pc}` in Mono JIT code
  (232K), Thumb SMLABB/SMULBB (207K), raw `svc 0` in libmono (59K), NEON VMOV immediate
  (~100K), VMAX/VMIN.F32 (75K).
- Each fallback copies ~100 registers into and out of Unicorn, so the audio thread holds
  the guest lock most of the time and the main thread waits (the 600 ms lock wait).
### E20. Translator: the missing instruction families
- Added (ARM and Thumb, ARM encoding shared): VMOV core<->scalar for 8/16/32-bit lanes
  (signed/unsigned to core), VLD1/VST1 one lane of 8/16/32 bits, VLD1 to all lanes,
  VMOVL/VSHLL, VCVT F32<->S32/U32, VMAX/VMIN.F32, all VMOV/VMVN/VORR/VBIC immediates
  (AdvSIMDExpandImm), SMLA<x><y> (Q flag set without touching host flags) and
  SMUL<x><y>, ARM STM with PC in the list (stores the STM address + 8).
- Self-test cases for each (ARM and Thumb functions), checked on the PC (Unicorn) for the
  expected values and on the phone with ARM32_CHECK=all against Unicorn.
- Phone: self-test 0 failures, checker 41476 blocks 0 differences, bench 144.7 ms (was
  197.6). Card Wars (USB; Wi-Fi ADB dropped and came back at 192.168.0.2): fallbacks
  ~11K/s (was ~150K/s); login -> LOAD_DATA within the 120 s, still 0.5-9 fps. New top
  sites: PKHBT (Thumb), a libunity matrix routine (VEXT, VREV64, VNEG.F32, VTRN, VZIP,
  VDUP scalar), the same family in ARM code at 0x019af3xx, `svc 0` in libmono.
### E21. Translator: permutes and PKH
- Added VEXT, VDUP (scalar), VREV64/32/16, VMVN, VABS/VNEG (integer and F32),
  VSWP/VTRN/VUZP/VZIP, PKHBT/PKHTB (ARM and Thumb). Self-test: a byte-pattern check of
  every permute (expected bytes computed in C) and PKH cases.
- Phone: self-test 0 failures, checker 41518 blocks 0 differences, bench 143.9 ms.
  Card Wars: fallbacks ~2.5-8K/s; start-up to LOAD_DATA in ~30 s (was 80+ s), but frames
  still 1.3-2.7 fps with 250-500 ms lock wait. simpleperf: 99.9% of samples in ONE thread
  (arm32-guest-108) running translated code (a ~1 KB host-code loop); every other thread
  asleep waiting for the guest lock.
### E22. Cause: no time slicing inside translated code
- Time slicing only happened in phone-function calls and YIELD. A guest loop with neither
  (a spin-wait on memory, or long pure computation) keeps the guest lock forever: on real
  hardware the threads would run side by side.
- Fix: a shared PREEMPT flag. A ticker thread (started the first time anyone waits) sets
  it when someone has waited and the holder has had the lock for a 2 ms slice. Linked
  block exits and jump-table exits check it (3 instructions) and go to the dispatcher
  instead; the dispatcher hands the lock over (share_gil). Taking the lock clears it.
- Self-test: a thread spinning with no calls and no YIELD must let the thread it waits
  for run (translator only; Unicorn has no slicing inside game code yet).
- Phone: self-test 0 failures incl. the new spin case, checker 41697 blocks 0 differences,
  bench 143.6 ms (no cost). Card Wars: UnityMain now gets CPU (profile: UnityMain 27%
  bridge + 26% translated, guest-108 9%); 3-7 fps, lock wait 60-160 ms.
- New profile: UnityMain spends ~20% in the dispatcher (engine::run, thread-local lookups,
  call_stub). Cause: blocks ending in NEON floating-point mode never linked (no_link), and
  the new NEON coverage put far more blocks in that mode. Fallbacks back to 12-16K/s: raw
  `svc 0` in libmono (6K/s), ARM `stmdb sp!, {sp, lr}` and RSC next to it.
### E23. Linking from NEON mode, raw system calls, RSC, STM {sp, lr}
- NEON-mode blocks now link: linked exits put host FPCR back in the guest's VFP mode first
  (3 instructions) instead of going through the dispatcher.
- `svc` outside the stub area (ARM, and Thumb outside IT blocks) ends the block with a new
  exit reason; the dispatcher runs the Linux system call on the translator's registers
  (no Unicorn round trip).
- RSC (ARM only; SBC with swapped operands) and STM with SP as base and in the list
  (stores keep the original SP; loads still go to Unicorn).
- Self-test: raw getpid via svc (ARM, Thumb), a loop whose block ends in NEON mode and
  jumps back to a VFP divide under round-toward-zero, RSC with both carries, push {sp, lr}.
- Phone (user: work on the phone only from now on): all new cases pass. The checker
  flagged the svc block: its Unicorn reference ran the system call itself (and would run
  it twice). Fixed: the reference stops before a raw svc, like the translated block.
- Card Wars still ~12-17K fallbacks/s; new sites are more NEON families: compares
  (VCEQ/VCGT/VCLT #0 ...), VPADD/VPMIN/VPMAX, VBSL, VRSQRTE/VRSQRTS, integer VMIN/VMAX,
  VLD2, VDUP from a core register.
### E24. NEON: the whole three-same and two-misc tables, VDUP core, VLD2-4/VST2-4
- Rather than one instruction at a time, every entry of the ARMv7 "three registers same
  length" and "two registers misc" tables that has a one-to-one ARM64 vector instruction
  (same lanes, same saturation, QC sticky flag in FPSR -> FPSCR) is now translated.
  VRECPS/VRSQRTS round the product first (ARMv7 and Unicorn), so they are done as
  FMUL + FSUB (+ *0.5), with the fused ARM64 instruction used only where that gives NaN
  (inf * 0, defined as 2.0 / 1.5). Also VDUP from a core register and the interleaved
  VLD2/3/4 and VST2/3/4 forms (via ld2-4/st2-4 into v16-v19 and lane inserts).
- Self-test: one function runs ~80 of these on mixed data (negative, large, inf, denormal
  floats) in ARM and Thumb; its checksum must match between ARM and Thumb, between the
  translator and Unicorn (ARM32_ENGINE=unicorn), and the checker compares every block.

---

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

# Packaging part A: one bridge build for every game (2026-10-12)
- Java natives are now connected at start-up (src/natives.rs): every `Java_*` export of the 32-bit library is
  demangled, its Java method's signature read through reflection, and RegisterNatives points it at a generated
  32-byte stub (movz x16,#index; ldr/br to one shared entry that saves x0-x7/d0-d7 and decodes by signature). The
  game's own RegisterNatives goes the same way. build.rs exports, tools/gen_exports.py and the per-game rebuild are gone.
- Helper libraries: one prebuilt stand-in (tools/standin/standin.c) reads class names from its original
  (lib<name>-arm32.so next to it), registers do-nothing natives for the app's own classes only (class loader != null),
  and skips methods the bridge reports as connected (arm32_bridge_connected). The main original is
  lib<main>-arm32.so; the loader prefers <name>-arm32.so for NEEDED libraries and only loads 32-bit ARM ELF files.
- The PC tool's pretend package name now comes from ARM32_PACKAGE (no game names left in src/ or tools/).
- Result, Puzzle Craft 2 repacked the new way: "[bridge] connected 47 native methods; 0 exports without a matching
  Java method", stand-in "1 do-nothing native methods in 1 classes" (same as the old generator), menu 119.8-120.1 fps,
  own code ~2 ms/frame; checker 1,048,576 blocks, 0 differences (30 ran past); self-test 0 failures on PC, and on the
  phone with jit, unicorn and ARM32_CHECK=all (35,328 checks, 0 differences).

# Packaging part B: second game, Puzzle Craft 1 (armeabi, cocos2d-x 1.x/2.x, 2026-10-12)
- tools/convert.sh (one step: detect armeabi/armeabi-v7a + main library, report missing imports via the new
  `arm32-bridge imports` command, sign, install over Wi-Fi). PC2: converted + installed with one command.
- PC1 report before: 44 missing (26 GLES1, 13 zlib, sem_*, getcwd, __gnu_Unwind_Find_exidx). Added: GLES1 from the
  NDK headers (212 generated + client array pointers by hand), built-in 32-bit libz.so from the NDK's libz.a
  (build.rs, used when a game needs libz.so and ships none), semaphores, getcwd, unwind alias, _chk forms, getauxval.
  Report after: 0 missing. PC CLI: load + JNI_OnLoad PASS.
- Phone crash 1: SIGSEGV in dynarec translate() reading guest pc 0xffff0fc0 = Linux kuser helper __kuser_cmpxchg
  (ARMv5 code has no ldrex/strex, so its atomics call the kernel's fixed-address helpers). Fixed generically: a
  KUSER page at 0xffff0000 with cmpxchg64/barrier/cmpxchg/get_tls (encodings checked with the NDK assembler) and
  version 5. Self-test check added; PC + phone (jit, unicorn, ARM32_CHECK=all: 40,370 checks, 0 differences) pass.
- Phone crash 2: "JNI DETECTED ERROR: mid == null in CallStaticObjectMethodA from Cocos2dxRenderer.nativeInit".
  Not the game: the bridge's own 120 Hz request called Cocos2dxGLSurfaceView.getInstance, which older cocos2d-x
  lacks (no "[bridge] Java has no method" line from the game's lookups, which are now logged). Fixed: every step
  checked; generic fallback through GLSurfaceView$GLThread.mGLSurfaceViewWeakRef for any GLSurfaceView game.
- After crash 2's first fix: PC1 title screen drawn (screenshot), 60 fps, game code ~1.2 ms/frame; one 3.5 s
  loading frame at start (own code 3.45 s; not investigated yet).
- Android shows its one-time "Review permissions" screen for targetSdk < 23 apps before the first start (user
  allowed test taps; tapped Continue).
- REA's Android tools need apktool/jadx (not installed); used aapt2/dexdump instead.
- 120 Hz: generic GLSurfaceView fallback works for PC1 ("asked for 120 Hz", display 120 Hz with a uid override),
  but PC1 still draws 60 fps: its Cocos2dxRenderer.onDrawFrame sleeps until animationInterval has passed (the
  game's own frame cap, set from native code). PC2 (cocos2d-x 3.x) has no such cap below 1/60 s. Put to the user.
- User chose to lift frame limits for all games. patch_dex.py fix 3: Thread.sleep inside any
  onDrawFrame(GL10) becomes nops. PC1: 116-120 fps (own code ~0.7 ms/frame); PC2 unchanged at 118-120.

# Packaging part C step 1: shared converter (2026-10-12)
- converter/ (apk-convert, Rust): picks the 32-bit ABI and main library, writes the APK with aligned stored entries
  (libraries 4096, others 4), Java fixes ported from patch_dex.py to Rust (own dex walker), missing-function report
  via arm32_bridge::imports::missing_imports. tools/convert.sh now builds + runs it, then signs and installs.
- Check: dex output byte-identical to patch_dex.py for both games (PC1 classes.dex + classes2.dex, PC2 both),
  zipalign -c -p 4 passes, missing counts 0 / 32 as before; PC2 converted this way runs at 119.9-120.2 fps.
- User: the phone app installs with root for now; may move to a non-root install later (keep that step separate).

# Packaging part C: Game Bridge phone app (2026-10-12)
- android/ (Compose, card grid = user's sketch C). Lists 32-bit-only APKs in Downloads (file access granted once
  through root appops), merges identical package+version, Convert / Open / Convert again. Runs apk-convert
  (shipped as libapkconvert.so), signs with apksig and the PC's debug key, installs with root
  (`pm install -r --bypass-low-target-sdk-block`) through a replaceable Installer.
- Check: on the phone, "Convert again" on Puzzle Craft 2 from Downloads (sha256 = original) converted, signed and
  installed in ~15 s with no PC involvement (lastUpdateTime 18:42:15); opened from the app: 119.9-120.3 fps, save kept
  (New Puzzlonia, LVL 4). Harmless SELinux denials: the converter's search of /data/local/tests.

# Debugging: Card Wars "NativeLoader.load failure, Unity libraries were not loaded" (2026-10-12)
## Symptom
Card Wars (com.ThugBoatLTD.CardWars 1.12.9, Unity 2022.3.33f1, Mono) converted in the Game Bridge app opens to
"Failure to initialize! Your hardware does not support this application. NativeLoader.load failure, Unity
libraries were not loaded." Every launch. Log: `[standin] .../libmain.so: 2 do-nothing native methods in 1 classes`
then `E Unity: NativeLoader.load failure`.
## Cause (from the log, the converter code and libmain's own imports)
- The converter put the bridge in the largest library (libunity.so) and the do-nothing stand-in in libmain.so.
  Unity's Java calls System.loadLibrary("main") and then NativeLoader.load (a native method in libmain); the
  stand-in returned 0 (false), so Unity showed the box. Sol's review found the same flaw independently (finding 3).
- libmain-arm32.so imports dlopen/dlsym and names libunity.so and libmonobdwgc-2.0.so: it loads the real game at
  run time. The bridge's guest dlopen only pretended (returned handle 1, loaded nothing), and dlsym searched one
  global table (libmain's own JNI_OnLoad would be found first). Puzzle Craft 1/2 only dlopen phone libraries
  (libGLESv2, libOpenSLES, ...), so the pretend was enough for them.
## Fix (user approved "make every part real", exact plan approved)
1. elf.rs: one shared loader; start-up code of each library runs once (dependencies first), never under the lock.
2. Guest dlopen really loads the game's own 32-bit library (lib<name>-arm32.so, full paths too) and runs its
   start-up code; handle = 0x100 + loader index; dlsym looks in that library first. Phone libraries keep handle 1.
3. Every game library becomes the stand-in; the bridge ships once as libarm32_bridge.so. The stand-in's JNI_OnLoad
   calls arm32_bridge_load(vm, env, original): load, connect Java_* exports, run its JNI_OnLoad. On failure it
   falls back to the do-nothing methods (old behaviour).
4. apk-convert / convert.sh: no --main; missing-function report covers every library.
5. Self-test: tests/guest/second.c (libsecond.so) opened by selftest with dlopen.
Check: PC self-test (translator + Unicorn) and cargo tests pass; PC1/PC2 reconverted keep ~120 fps and saves;
Card Wars log shows libunity loaded and its JNI_OnLoad ran, no "Failure to initialize".
## Attempts
### 1. Shared loader + real dlopen + every part a stand-in (the approved fix)
- PC: cargo build clean; self-test 0 failures on the translator and on Unicorn, incl. 4 new dlopen checks; cargo
  tests pass.
- Puzzle Craft 2 (reconverted, adb install, started from the PC with monkey, no taps): libcrashlytics now really
  loaded (JNI_OnLoad 0x10006; it opened libcrashlytics-envelope.so with the real dlopen; mmap has no handler and
  returns 0), libcocos2dcpp 47 methods connected, 119-120 fps for 20 s, save kept (New Puzzlonia LVL 4).
- Puzzle Craft 1: SIGSEGV on GLThread in Java_..._nativeOnPause (0x10cc62: bl to a shared-instance getter that
  returns 0, then ldr [r0]) before nativeInit. Android relaunched the activity at launch (wm_relaunch_resume_activity,
  config 0x400 = screen size) only in the monkey launches, never in the user's earlier taps. A/B: the OLD converter
  and bridge from the installed Game Bridge app, same monkey launch -> identical fault (same block 0x0110cc69, same
  registers). Not caused by this change: the game's own pause-before-init bug, triggered by the relaunch.
- Card Wars: libmain-arm32 loaded through its stand-in (JNI_OnLoad 0x10006), its NativeLoader.load now runs and
  opened libunity.so through the real dlopen. Then libunity's start-up code called getpagesize (no handler,
  returned 0) and read address 0. Next blocker = missing phone functions (369 listed by the converter), not loading.
- Conclusion: the loading fix works (Card Wars gets past "Failure to initialize"). Card Wars needs more phone
  functions next; stopped here to ask the user.
### 2. Review fixes, PC1 pause guard, Card Wars step 1 (approved plans)
- PC1 pause guard (app.rs forward): until a *_nativeInit has returned, *_nativeOnPause/OnResume are skipped, only
  in games that have a nativeInit. Same monkey launch (Android relaunched the activity again): 3 calls skipped,
  title reached, 119.9-120.1 fps, save kept ("Test", Lvl 2).
- Review fixes: JNI names starting with _ (unit tests added); a re-registered method keeps its stub, stub memory
  grows in 64 KiB blocks (no 2,048 cap); converter turns android:extractNativeLibs false -> true (checked on a
  Card Wars copy flipped to false: aapt2 shows true after converting); glMultiDrawElementsEXT converts the 32-bit
  index pointer list, glExtGetBufferPointervQCOM returns NULL, glMapBufferOES starts from the buffer's contents
  (glMapBufferRange read; zero + clear error if that fails); Game Bridge conversion state lives in a process-wide
  object; getcwd(NULL) allocates; the sound list creates files/.
- @CriticalNative: not done. Its annotation has CLASS retention, so reflection can't see it at run time; put to the user.
- Card Wars step 1: getpagesize, gettid, uname ("armv8l"), getrlimit, pthread_rwlock (6), pthread_condattr (3,
  monotonic clock remembered per condition variable), pthread_attr (7, detached honoured by pthread_create),
  pthread_getattr_np (real guest stack), sched (5), pthread_get/setschedparam, pthread_setname_np.
  Missing list 369 -> 342. PC tests pass; PC2 120 fps.
- Card Wars now stops at libunity's start-up: mmap (no handler, returned 0) then munmap, then a read of address 0.
  mmap is on the "stop and ask" list (memory mapping) -> asked the user.
### 3. Memory blocks (approved) + @CriticalNative warning (approved)
- mmap/mmap64/__mmap2 (page-aligned guest heap blocks; file mappings are private copies), munmap (whole blocks),
  mprotect (logs when non-library memory is made executable), madvise (DONTNEED zeroes). Converter prints
  "warn ... @CriticalNative" (checked: only the copy with the marker added warns). PC tests pass; PC2 120 fps.
- Card Wars: libunity, then libmonobdwgc opened by game code, "Mono: JNI_OnLoad called", Unity main thread starts,
  Unity logs "MemoryManager: Using 'Dynamic Heap' Allocator." Then a black screen; process alive, one guest
  pthread (arm32-guest-100) at ~100% CPU. Last missing functions hit: sigemptyset, ANativeWindow_fromSurface,
  newlocale, fscanf, syscall, prctl (all returned 0). Suspect (not yet proven): Unity's locks use raw
  syscall(SYS_futex); a wait that returns 0 at once makes a waiter spin. ANativeWindow/EGL = Unity's own screen
  setup, on the stop-and-ask list. Stopped and asked.
### 4. Keep going on Card Wars (user: "keep working, no need to ask")
- Futex: syscall(SYS_futex) WAIT/WAKE/BITSET on guest words (generation counter + wait_for_guests). The spinning
  thread stopped. One linux_syscall serves libc syscall() (-1 + errno) and raw `svc` (raw -errno in r0).
- __memcpy_chk and friends (_chk aliases) were missing: Unity called exit(1) after one returned 0. Added.
- Mono's JIT writes code into heap memory and flushes with ARM cacheflush (svc 0xf0002): the translator now drops
  the blocks in that range (unlinks incoming jumps, resets jump-table entries) and Unicorn drops its cache; the heap
  is executable. Unity then logs "Company Name: ThugBoatLTD / Product Name: Card Wars"; process alive.
- Batch of small functions (ctype, wide chars, math, clock, file state, locale, signals).
- Next blocker: ANativeWindow_fromSurface and all of EGL had no handler. Added egl.rs: EGL, ANativeWindow and
  ALooper with small handles for 64-bit host pointers (same host pointer -> same handle). Added GLES 3.0-3.2 to the
  generator (555 GL functions): index/indirect/pixel/vertex pointers are buffer offsets when a buffer is bound for
  that target (bindings tracked per target), glMapBufferRange uses a guest copy (read back unless invalidated,
  uploaded on unmap or flush), GLsync handles, pointer-array functions by hand, debug callback ignored.
- With EGL in: no more ANativeWindow "no handler". New stop: right after "no handler yet for pthread_kill", every
  guest thread asleep (0% CPU). Three clues agree on Mono's garbage collector pausing threads: libmonobdwgc imports
  pthread_kill + sigaction + sigsuspend + pthread_sigmask; Mono on Android pauses with SIGPWR and resumes with
  SIGXCPU, waiting on a semaphore for each answer; the bridge dropped every signal, so the answer never came.
- Added signals.rs: per-signal actions, per-thread blocked/pending sets, pthread_kill/raise/kill, sigsuspend,
  pthread_sigmask/sigprocmask. A signalled thread runs the handler (siginfo + ucontext with its registers, pc =
  return address of the phone function it is parked in) when it next holds the guest lock: before/after a wait,
  after without_gil, after a phone function. First self-test hung: the signal arrived before the worker started
  waiting and the wait slept without checking -> waits now check first. Self-test 0 failures on the PC (new check:
  pause/resume another thread like Mono, blocked signal held until unblocked).
- Unity then asked eglGetProcAddress for eglGetFrameTimestampSupportedANDROID and called it unchecked (jump to 0):
  added the 8 EGL extension functions libunity names (sync fences, frame timestamps, presentation time). Jumps to
  non-code now stop with a guest fault naming the jumping block instead of crashing the translator.
- Weak imports nobody defines now resolve to 0 like the real loader (was: a stand-in that returns 0), e.g.
  _ZTH15gDeferredAction (C++ thread_local init hook). Missing list 226 -> 198. Did not change the next crash.
### 5. Crash: callee-saved registers wrong after a call (libunity 0x107f504, r6 = 0 although checked non-zero)
- Same registers every run (not timing). Signal deliveries logged with before/after registers: unchanged.
- Unicorn-only and checker runs first looked "fine" but the notification shade was open (game paused): void.
  cw-run.sh now waits while the shade is open.
- Checker (sampled) with the game in front: 51 differences in 2 blocks. (a) STREX right after LDREX in an earlier
  block: checker blind spot (Unicorn has no monitor), not a bug. (b) ARM `tst r12, r2, lsr r6` with r6 = 0:
  translator cleared C, real ARM keeps it. Cause: arm.rs passed T2 as the result register to shift_register,
  which keeps the amount in T2 for its "amount was 0" check -> the check tested the result instead. Fixed (T6),
  assert added, self-test case added (6 shift/carry cases): passes on PC (Unicorn) and phone (translator).

# Parallel guest threads (no guest lock in translator mode), progress record (2026-10-13)
- Approved plan: `/tmp/a32k/parallel-plan.md` (copy `~/.cache/parallel-plan.md`): v2 + v3 (R1-R11) + v3.1 (G1-G9).
  User choices: everything parallel, fix P1 (signals to threads stuck in host calls) and P2 (GLES state per
  context) now. General fixes only. Sub-agents from 2026-10-13 03:1x: Codex Sol 6.1 high (not fast; out of credits until 04:34) or via `crush spawn --cli opencode`: opencode-go/mimo-v2.6-pro or glm-5.3-flash (reviews read-only). UPDATE 2026-10-13: sub-agents now Claude Sonnet 5.5 high, or OpenCode GLM 5.3 Flash (several at once) / MiMo 2.6 Pro (slow); Claude Fable 5.1 high only for the FINAL review or an absolutely critical one.
- Checkpoint commit before this work: 0a733ae. Everything since is uncommitted.
- State: all mechanisms built; parallel on by default (ARM32_PARALLEL=0 turns it off).
- Phone run before the review fixes (job 263): default, LSE=0, PARALLEL=0, unicorn: 0 failures. Counters exact,
  1000/1000 signals, blocked read 3 ms, busy threads 181/300 ms (was 4429 ms). FORCE_FALLBACK_SYNC=1: 600 s timeout,
  blocked read 115 ms (limit 100). CHECK=all: not finished (earlier: 20 differences in rewritten code at
  0x10001000, checker ran a stale Unicorn copy; checker fixed, not rerun).
- Blocked-read slowness: helper picks the job up in 0.3-2.4 ms, but delivery took 1.2 ms (default), 51 ms (LSE=0),
  111 ms (forced fallback). Cause unknown; TEMP timing split added in deliver_for (engine / entered / handlers).
- Thread-core review (Fable, t5) fixes applied 2026-10-13: #1 call_guest_native returns to IN_GUEST when called
  from inside guest code; #2 deliver_pending_on enters guest first (waits out a helper's borrow); #4 Identity
  `exiting` blocks new borrows at thread exit; #5 fence after clearing attention; #6 no wake for a blocked signal
  (pending/blocked handshake now SeqCst both sides); #7 new engine sets attention if a signal is pending;
  #8 helper requested for any other-thread target. (#3 fixed earlier.)
- Translator review redo (t6 failed on Fable's limit): Gemini task t8, diff `/tmp/a32k/translator.diff`.
- Test scripts: `/tmp/a32k/excl-test2.sh` (phone build + six self-test modes, per-mode time, outputs
  `/tmp/a32k/st-<mode>.out`), `/tmp/a32k/games-check.sh` (Puzzle Craft 2 + Card Wars 180 s, launch only).
- 2026-10-13 slow helper delivery explained. TEMP split on the phone: handlers take ~1 ms; the time is the
  helper's new Unicorn engine. Every new engine (each guest thread + each helper) spends 10-350 ms in its first
  mem_map (Unicorn's lazy machine init); worse when several threads start at once. PC: ~2 ms.
  simpleperf (dwarf): uc_init_engine -> arm_cpus_init -> register_cp_regs_for_features ->
  define_one_arm_cp_reg -> add_cpreg_to_hashtable: thousands of tiny g_memdup/strdup mallocs; the phone's
  hardened malloc (GrapheneOS: random slots, chacha, per-size-class locks, mprotect) makes each slow and
  serialises threads. Explains: phone-only, grows with concurrency, fast when a helper engine is reused.
- Forced-fallback mode is slow, not hung (simpleperf: ~45% in uc_reg_read/uc_reg_write copying ~55 registers
  each way per fallback; no stop-the-world spinning).
- 2026-10-13 phone run (review fixes, before pool/batching): default 0 failures (blocked read 89 ms: new helper
  engine 85 ms); CHECK=all 0 failures, 1374425 blocks checked, 0 differences (stale-code checker fix confirmed);
  FORCE_FALLBACK_SYNC=1 still 600 s timeout (slow fallbacks, profiled).
- User chose (2026-10-13): engine pool (finished threads' engines reused, 2 spares built ahead by a builder
  thread, keep at most 4) and one batch Unicorn register call each way per fallback. Both implemented; PC
  self-test 0 failures.
- Translator review (Gemini 3.8 Flash high, t8, 2026-10-13): exclusives, barriers, attention checks, Unicorn
  patch and checker judged correct. Two findings, both confirmed in code and fixed: (1) HIGH invalidate() unpatched
  links under stop-the-world without the cross-core sync the plan requires -> sync_other_cores() at the end of
  the drop; (2) MEDIUM with linking off new blocks were never synced -> sync_other_cores() on that path too.
- 2026-10-13 final round (pool + batch + all review fixes): default, CHECK=all (1348993 blocks, 0 differences),
  LSE=0, PARALLEL=0, unicorn: 0 failures. Whole runs much faster (unicorn 32->7 s, PARALLEL=0 57->16 s, LSE=0
  20->4 s); blocked read 4-5 ms. FORCE_FALLBACK_SYNC=1: still hangs in the counter test even at 2000 rounds.
- Forced-mode hang evidence: per-thread fallback samples: all four threads at the SWP spinlock (pc 0x1017570,
  lock 0x1029afc), every swap returns 1, ~1630 of 2000 rounds done -> lock reads taken while nobody holds it
  (lost release). Ruled out: Unicorn serial "step atomic" (our patch stops with an error on EXCP_ATOMIC; all
  sampled fallbacks returned Ok). Testing: SWP executed twice (acquire, then re-run reads its own 1).
- Forced-mode hang, more evidence: FORCE_FALLBACK_SYNC=1 with PARALLEL=0 passes (0 failures) -> parallel-only.
  Re-run detector (did a thread swap again before its critical section ran, by the test's own counter just
  before the lock): never fired -> SWP re-execution ruled out. Unicorn engines do get the parallel flag (reg 143
  in build_engine; not touched by context restore). Next: count acquisitions vs finished critical sections at
  the hang. Phone was swapping heavily (Ente Photos 3.6 GB, swap full) during these runs: timings unreliable.
- Hang accounting (07C, forced mode, parallel): all 4 threads hung at SWP, acquisitions 1361 = critical sections done 1361,
  lock reads 1. Every taken lock was released, yet the lock is set: some SWP wrote 1 over 0 while its thread got 1 back
  (swap ran twice, or its result was lost). Unicorn builds every block with CF_PARALLEL (checked uc_gen_tb, cpu_exec,
  cflags_next_tb writers), so the swap itself is atomic. Next: TEMP per-thread counter inside Unicorn's xchg helper
  (atomic_template.h + uc.c) to log fallbacks where the helper ran != 1 times or its old value != guest result.
- TEMP helper counter (forced mode, parallel): 0 mismatches (helper ran exactly once per SWP fallback, its old value =
  guest result), yet hang again (acquisitions 812 = critical sections 812, lock 1). So each swap was honest but not atomic.
- ROOT CAUSE (forced-mode hang): vendor Unicorn atomic_template.h xchg is `ret = *haddr; *haddr = val;` (plain read,
  then write) on all compilers (Unicorn's MSVC port change; upstream QEMU uses atomic_xchg__nocheck). The lock owner's
  release store (translated `str r1=0`) landing between a spinning thread's read (1) and write (1) is lost; lock stays
  1 forever. Explains: only parallel (PARALLEL=0 passes), only when SWP runs in Unicorn (forced fallback), every acquire
  released. min/max helpers had the same flaw (compare then plain write). Fix: atomic_xchg__nocheck /
  atomic_cmpxchg__nocheck (patch 10 in vendor ARM32-BRIDGE.md). All TEMP code removed.
- 2026-10-13 round after the xchg fix (no TEMP code): phone all six modes 0 failures: default 18 s (busy 145/153 ms),
  FORCE_FALLBACK_SYNC=1 22 s (was hung 600 s; counters exact 8000), CHECK=all 195 s (1339148 blocks, 0 differences),
  LSE=0 23 s, PARALLEL=0 50 s, ENGINE=unicorn 29 s. PC release build clean; PC default + forced mode 0 failures.
- MiMo 2.6 Pro review (t10) of pool / batch copy / signal fixes: all three correct. Findings, each checked in code:
  (1) MEDIUM deliver_for cleared fault but not exit_code; exit() here only ends the guest call, and helpers keep their
  engines, so one exit() in a helper-run handler broke every later delivery -> now cleared too. (2) LOW on_interrupt's
  raw-syscall path (svc inside Unicorn) skipped park_registers/leave_guest, unlike call_stub, so a futex wait there
  counted as in guest -> now clear_exclusive + park + leave/enter around it. (3) LOW PooledEngine Send over Rc-based
  Unicorn: sound because an engine is in the pool or one thread's slot, never both -> comment added, no code change.
- NEW HANG (default mode, 2026-10-13 04:07, build with MiMo fixes): stuck after "semaphores" in the SIGPWR pause/resume
  test, 0.4% CPU. Threads: main + arm32-guest-101 (the worker) both in futex wait on heap condvars (parkers), signal
  service idle, NO helper thread ever started. So the worker slept in sem_wait with SIGPWR undelivered: a lost signal
  wake-up. Earlier runs 068/078 (200 s limit) also stopped right after "semaphores", blamed then on memory pressure,
  so this is likely pre-existing and intermittent (worse when the phone is slow). Added TEMP dump (file `dump-now`
  in the working dir -> logs identities, runners, waiters) and /tmp/a32k/hang-loop.sh to reproduce.
- Repro loop (hang-loop.sh) hung on run 7 of 20. Dump: worker 0x101c in on_pause -> sigsuspend (mask = all but SIGXCPU),
  pending 0, delivered 1; main 0x1004 waiting on paused_answer (2nd sem_wait). delivered counts FINISHED handlers, and
  on_pause has not returned, so delivered 1 = on_resume already ran. Cause: TEST RACE, not a bridge bug. on_pause had an
  empty sa_mask, so SIGXCPU could be delivered (block exit / phone-call return, same as a kernel's delivery points)
  after `while (!resumed_flag)` read 0 and before sigsuspend's `before` count -> sigsuspend waits for a signal already
  handled. Same race on real Linux. Mono blocks all signals in its suspend handler (sigfillset sa_mask). Fix: test sets
  sigfillset(&action.sa_mask) for SIGPWR. Earlier 068/078 "stopped after semaphores" were the same hang. TEMP dump removed.
- FINAL review, Fable 5.1 high (t11): no HIGH left. (1) MEDIUM cacheflush ran invalidate (STW) then note_code_change,
  so between them another thread's Unicorn could run its old TB of just-patched code -> order swapped (fixed).
  (2) MEDIUM guest stacks never reused: bump allocator, 256 MiB, ~247 pthread_creates per session then assert/abort.
  Pre-existing; not fixed yet (asked the owner). (3) LOW raw svc in Unicorn did not set RESUME_AT -> handler context
  PC = LR -> set like native_syscall (fixed). (4) LOW checker mode only: reused engine gets duplicate checker hooks
  (results stay right, hooks pile up per reuse) -> not fixed yet (asked).
- Test race fix verified: hang-loop 20 runs on the fixed selftest (see next entry).
- 2026-10-13 04:25 build (xchg fix, MiMo fixes, Fable fixes 1+3, test sa_mask fix, no TEMP code): hang-loop 20/20 ok
  (was hung on run 7 before the test fix); phone six modes 0 failures (default 21 s, forced 19 s, CHECK=all 185 s with
  1347771 blocks / 0 differences, LSE=0 21 s, PARALLEL=0 48 s, unicorn 24 s); PC build clean, PC default + forced 0 failures.
- Owner chose: fix the stack leak now, checker hooks later (TODO: per-engine "checker hooks installed" flag kept
  through idle_thread, so a reused engine in ARM32_CHECK mode does not add them again).
- Stack leak fix: new self-test "300 threads one after another" (create + join). Before the fix, PC: panicked at
  runtime.rs:286 "out of guest stack space", then hung (timeout). Fix: Shared.free_stacks; drop_engine (after the guest
  thread has finished and woken its joiners; helpers are done before it) pushes the thread's 1 MiB stack top (not
  thread 1's main stack); alloc_stack reuses a freed one first.
- Stack fix results: PC release build clean; PC default + forced 0 failures, "many threads: 300 of 300 ran".
  Phone round NOT run yet: USB link gone (hours later), wireless ADB connected once (192.168.0.2:34559) then pushes
  failed (EOF) and the phone went offline / stopped advertising. Asked the owner to reconnect.
- 2026-10-13 phone round with the stack fix (USB): all six modes 0 failures, "many threads: 300 of 300 ran" in every
  mode; default 15 s, forced 15 s, CHECK=all 155 s (1352627 blocks, 0 differences), LSE=0 15 s, PARALLEL=0 37 s,
  unicorn 21 s.
- Review of stack reuse etc., Sonnet 5.5 high (t12): stack reuse sound (helper/exit ordering, double push, lock order,
  join race checked). (1) MEDIUM thread ids never reused + lock owner stored as id << 16: with stacks reused, churn is
  unbounded, id 65536 wraps to "no owner" -> two threads in one lock. Fixed: Shared.free_threads keeps (id, stack top,
  errno slot, TLS block) of finished threads; adopt_engine reuses the whole set (TLS block and errno slot zeroed).
  (2) LOW RESUME_AT set to None after use, losing an outer value (syscall inside a handler run by deliver_for /
  safe_point) -> all five sites now restore the previous value. (3) LOW test did not prove reuse -> each thread returns
  __builtin_frame_address(0); test counts distinct 1 MiB stacks (< 50 of 300 required; later < 150). (4) LOW reused stack not
  zeroed -> left as is (bionic thread stacks also hold old data when cached).
- Owner 2026-10-13: Sol 6.1 high sub-agents allowed again (not fast).
- Sol 6.1 high review (t13) of thread reuse + RESUME_AT + test: id reuse safe (joins/signals/waiters use Identity.handle;
  runner removal by pointer; helpers done before slots are recycled; TLS zeroing matches a fresh block). (1) MEDIUM a
  signal raised inside a handler got the OUTER resume point (call_stub did not set RESUME_AT; true before today too
  unless a syscall had cleared it) -> call_stub now scopes RESUME_AT = its LR through the call and its delivery, then
  restores. (2) LOW stack count could exceed 50 by scheduling (join returns before drop_engine) -> limit now < 150.
  (3) LOW a faulting worker returns 0 and could fake reuse -> null frame address fails the test.
  Previous revision (id reuse) on PC: "300 of 300 ran, on 1 stacks", 0 failures both modes.
- FINAL verification 2026-10-13 (all fixes, no TEMP code): PC release build clean, PC default + forced 0 failures
  (300 threads on 1 stack). Phone six modes 0 failures: default 15 s (300 threads on 2 stacks), forced 10 s,
  CHECK=all 131 s (1343614 blocks, 0 differences), LSE=0 16 s, PARALLEL=0 37 s, unicorn 17 s (each 1 stack).
  Open for later: checker hooks re-added on reused engines (owner chose later); reused stacks not zeroed (by design).

## 2026-10-13 15:00 — game checks on the phone with today's bridge (parallel threads, engine pool, atomics fix)

Converted and installed each game with `tools/convert.sh` (rebuilds `libarm32_bridge.so`), opened with no taps, read `[fps]` from logcat, closed.

- **Card Wars** (180 s): alive after 180 s (pid 13056), no fatal/fault lines. Unity reached `READY` at 14:57:47 (about 15 s after launch).
  - fps per minute: 14:57 avg 53.0 (loading; slow seconds 1.8-23.8 only before READY), 14:58 avg 72.2 (min 43.5), 14:59 avg 72.1 (min 69.1), 15:00 avg 71.4 (min 65.7). Median 72.2 over 172 samples.
  - Steady frame: game code ~13.5-15 ms (own code ~9.3-10.3 ms, phone functions ~4.1-4.9 ms), guest-lock wait 0.00 ms, 26761 stop-all-threads events, 3 signal helpers.
  - Before today: ~0.5-1.5 fps on the phone (~9.5 under Unicorn). About 50-100x faster.
  - `dumpsys gfxinfo` printed nothing (Unity draws to its own surface), so the bridge `[fps]` line is the only fps source.
- **Puzzle Craft 1** (45 s): 119.9-120.0 fps, game code ~6.7 ms. Save files unchanged.
- **Puzzle Craft 2** (45 s): 105.9-120.1 fps (one 84 ms slow frame), game code ~6.6-8.1 ms. Save files unchanged.
- gfxinfo for Puzzle Craft counted only 3-5 frames (cocos2d also draws to its own surface), so its janky numbers mean nothing.

Next idea for Card Wars: own code is ~2/3 of the frame; the remaining limit to 120 fps is translated guest code, not locks.

# What's left (2026-10-13, after commit 06c499c)

State: translator is the default; guest threads run in parallel. Phone results with 06c499c:
Card Wars ~72 fps steady after loading (idle, no taps), Puzzle Craft 1 ~120, Puzzle Craft 2 106-120.
Self-test: 0 failures in all six modes on the phone, default and forced modes on the PC.

## Rule for all remaining work
Engine work only: no code that checks a game's name, package, library hash or fixed address, and no change
aimed at one function of one game. A fix must be a general bridge/translator change. After every speed or
correctness change, check all three games (Card Wars, Puzzle Craft 1, Puzzle Craft 2) plus the self-test.

## 1. Card Wars to full speed (main goal)
- Now: 70-74 fps; game code 13.4-15.2 ms/frame = own code (translated guest code) 9.2-10.3 ms + phone functions
  4.1-4.9 ms; guest-lock wait 0.00 ms; "stops of every thread" stays at 26761 after loading (no new stops in
  steady state).
- Goal: 115+ fps average over 30 s (display is 120 Hz), i.e. game code <= ~8 ms/frame, no stall > 100 ms after
  loading. Same bar as docs/translator.md pass check 6.
- Not done yet: a steady-state profile of Card Wars on the translator. First step: simpleperf on the running game
  (`su -c 'simpleperf record -p PID --duration 15 -f 2000 -g -o /data/local/tmp/cw.perf'`, all threads), split time
  into translated code, dispatcher/block lookup, call_stub + phone-function handlers, Unicorn fallbacks, exclusive
  monitor (LDREX/STREX), signal delivery, kernel. Pick the change from the profile, not from a guess.
- Suspects to check against the profile (none proven): Mono JIT code reached by indirect jumps that miss the
  jump table; Unicorn fallbacks for instructions the translator lacks; per-call cost of the phone-function path
  (how many calls per frame); exclusive-monitor cost now that threads are parallel; GL wrapper copies.

## 2. Card Wars loading
- ~15 s from launch to Unity "READY"; seconds at 1.8-24 fps while loading; 26761 stop-every-thread events, all
  during loading. Profile the loading window too (likely Mono JIT writes + cacheflush invalidations).

## 3. Phone functions with no handler that the games really call (from today's logs)
- Card Wars: setpriority, getrusage, ASensorManager_getInstance. Puzzle Craft 2: ASensorManager_getInstance,
  AConfiguration_new. Each returns 0 today. (The converter's longer lists, 162 and 29, are imports that may never
  be called.)

## 4. Smaller open items
- Own-code spikes: Puzzle Craft 2 had one 84 ms frame (own code 71 ms); older note "own code 45-59 ms every few
  seconds, unknown". Not explained.
- Checker mode: checker hooks are added again on reused pool engines (duplicate hooks). Owner chose "later".
- Reused thread stacks are not zeroed (by design, like real pthreads). Keep.
- Checker blind spot: STREX right after LDREX in an earlier block (Unicorn has no monitor). Not a bug.
- Card Wars was only tested idle (no taps). Play, sound and touch are untested; taps need the owner's permission.

## How to measure (scripts lived in /tmp/a32k, which gets cleaned; rebuild from these)
- Convert + install one game (rebuilds libarm32_bridge.so itself):
  `tools/convert.sh <apk> --out /tmp/a32k/<name>-arm64.apk --device 56280DLCR0026M`
  APKs: Card Wars `~/dev/arm32-bridge-games/cardwars/CardWars.apk` (com.ThugBoatLTD.CardWars);
  Puzzle Craft 1 `~/pixel10-flash/games/puzzlecraft1/com.chillingo.puzzlecraft.android.rowgplay.apk`;
  Puzzle Craft 2 `~/pixel10-flash/games/puzzlecraft2/com.chillingo.puzzlecraft2.android.gplay.apk`.
- Run: `adb shell am force-stop P; adb logcat -c; adb shell monkey -p P -c android.intent.category.LAUNCHER 1`,
  wait (Card Wars 180 s, Puzzle Craft 45 s), `adb logcat -d > log`, `adb shell pidof P`, force-stop.
  Read `grep 'fps\]' log` (tag arm32bridge, one line per second), faults with
  `grep -E 'F libc|FATAL|\[fault\]|panick|no handler yet'`, Card Wars state with `grep 'Changing State'`.
- dumpsys gfxinfo is useless here (games draw on their own surface); use the [fps] lines and SurfaceFlinger
  timestats.
- Puzzle Craft saves: list `/data/data/P/files` and `shared_prefs` with `su -c 'ls -la --time-style=full-iso ...'`
  before and after; they must not change.
- Self-test, phone, six modes: build with `systemd-run --user --wait --pipe --quiet -p AllowedCPUs=6-11,18-23
  --working-directory=$HOME/dev/arm32-bridge ./build-phone.sh`, push `target/aarch64-linux-android/release/arm32-bridge`
  and `tests/guest/build/{selftest,libsecond,libslowinit}.so` to /data/local/tmp/a32, run
  `./arm32-bridge call selftest.so run_all` with each of: (none), ARM32_FORCE_FALLBACK_SYNC=1, ARM32_CHECK=all,
  ARM32_LSE=0, ARM32_PARALLEL=0, ARM32_ENGINE=unicorn. 0 failures required in every mode.
- Self-test, PC: `cargo build --release` under the same systemd-run wrapper, then
  `./target/release/arm32-bridge call tests/guest/build/selftest.so run_all` (also with ARM32_FORCE_FALLBACK_SYNC=1).
- Noise: run the unchanged build at least 3 times before judging a change; compare whole runs.

# Card Wars speed + crashes, session 2026-10-13 afternoon (start: commit 06c499c, unchanged build)

## Baseline (unchanged build, 3 launches, no taps, 120 s each, steady = from READY+15 s)
Scripts (outside the repo): `~/dev/arm32-bridge-games/scripts/` (cw-measure.sh, fps-summary.py, cw-profile.sh,
perf-split.py, perf-ips.py, cw-crashloop.sh); raw logs in `~/dev/arm32-bridge-games/runs/`.
- base1: 71.7 fps avg (min 65.0), game 13.92 ms, own 9.48, phone 4.44; 30 s windows 71.5 / 72.2 / 71.5
- base2: 72.5 fps avg (min 64.4), game 13.78, own 9.37, phone 4.41; windows 72.0 / 72.7 / 72.8
- base3: 72.7 fps avg (min 66.8), game 13.74, own 9.33, phone 4.41; windows 72.7 / 72.6 / 72.9
- Noise: whole-run averages within 1.0 fps (71.7-72.7). READY 19-20 s after launch.
- Game code per frame (13.7-13.9 ms) is the whole frame at 72 fps: the main thread is CPU bound.

## Steady-state profile (prof1: simpleperf 2000 Hz, 15 s, 60 s after launch, all threads, no call graph)
- By thread (share of all samples): guest workers "arm32-guest-103x" 35.4%, UnityMain 23.1%, "102x" 19.1%,
  "108x" 17.4% (thread names are cut to 15 chars, so each covers up to 16 guest threads). The worker pool
  burns ~2.5 cores: ~42% translated code (spinning), ~27% kernel futex wake/wait (_raw_spin_unlock_irqrestore),
  ~25% bridge.
- UnityMain (86% busy): translated code 43.2%, bridge 48.1% (engine::run 26.5% self, thread_local get 8.6%,
  call_stub 6.1%, park_registers 2.9%, RwLock read_contended 1.2%), vdso clock_gettime 3.0%, kernel 4.4%.
- Inside engine::run on UnityMain (6839 samples), hot spots (llvm-objdump of the same build):
  0x1435ac 1453 + 0x14358c 648 (block-cache RwLock read acquire: ldaxr/stxr on the shared lock word),
  0x14362c 1629 (hash map bucket load in block_for), 0x1436f0 1663 (RwLock read release: ldxr/stlxr).
  => ~80% of the dispatcher's own time is the block-cache read lock + hash lookup done on EVERY dispatcher
  entry, and the lock word is shared by all threads (cache-line ping-pong between cores).
- Conclusion: the main thread loses ~half its time outside translated code; the dispatcher lookup is the
  biggest single piece. Next: count dispatcher entries and why they happen (jump-table misses with a
  4096-entry table, phone-function calls/returns, fallbacks) before choosing the speed change.

## NEW: Card Wars crashes during loading (unchanged build)
- prof1 launch: abort 13 s after launch on UnityMain: `* Assertion at class.c:4575, condition
  m_class_get_rank (ac)' not met` (Mono), while loading card data (tombstone runs/prof1.tombstone).
- Crash loop, parallel (default), 10 launches x 40 s: 8 alive, 2 dead:
  run 3 `Assertion at ../../mono/metadata/handle.h:681, condition m_class_is_valuetype(...)`;
  run 5 `[fault] bad guest memory access at 0x00000041 in translated code (guest block 0x026414e4)` =
  inside libmonobdwgc (0x0221f000-0x02861000), r0=0x31 (garbage object pointer).
  Total with prof1: 3 dead of 14 launches (~20%). All during loading (before READY).
- Crash loop, parallel off (debug.arm32bridge.parallel=0): loading is so slow that no run reached READY in
  40 s, so this loop cannot show the loading crash either way. Not usable as evidence.
- Each loading run shows 1000-2400 pairs of "[signal] 30 ran" / "[signal] 24 ran" (SIGPWR = Boehm GC
  suspend, SIGXCPU = restart): one stop-the-world GC pause of every thread many times per second.

### Suspects (ranked)
1. Signal frame not on the guest stack (most likely). run_handler (src/imports/signals.rs) puts the
   siginfo + ucontext (all 15 registers of the interrupted code) in a heap block (heap().malloc), and the
   handler runs on the thread's stack 256 bytes below sp. On Linux the kernel writes that frame onto the
   thread's own stack. Boehm GC's suspend handler records its own sp (GC_approx_sp) as the top of the
   thread's stack to scan, and relies on the full register context being in the signal frame on that
   stack (bdwgc pthread_stop_world.c: "We believe that in this case the full context is already in the
   signal handler frame."). The handler's prologue saves only r4-r11; a GC pointer that lives only in
   r0-r3, r12 or lr at the moment of the pause is in the heap block, which the GC never scans -> the
   object is freed while in use -> later Mono asserts / bad reads. Fits: intermittent (needs a pointer
   only in a scratch register at a pause), loading only (thousands of GC pauses + heavy allocation),
   different symptom each time (whatever object got freed), parallel mode (signals now land at any
   block boundary in the middle of guest code via the attention flag, where scratch registers are live;
   with the guest lock they were only taken at phone-function returns).
   Evidence: (a) code: heap frame; (b) bdwgc source comment; (c) runtime: Mono object corruption only
   during GC-heavy loading.
2. Literal values baked into translated code (thumb.rs load_store, "Literal pools live in code, which never
   changes"): true for library files, not for code Mono's JIT writes and later patches; a cache flush only
   drops blocks that overlap the flushed bytes, and a literal usually sits just past its block's end. Would
   make calls go to stale targets. General bug, but crashes are in libmonobdwgc / Mono runtime asserts, not
   in JIT code, so it explains the symptoms less well. Fix separately after suspect 1.
3. Translator race in parallel mode (exclusives are CAS-based: ABA not noticed). Mono/Boehm use CAS-style
   loops, which have the same ABA behaviour natively. Unlikely.
- Parallel-off loop final: 10 alive, 0 dead, but 0 of 10 reached READY in 40 s (loading too slow), so it
  says nothing about the loading crash.

### Attempt 1 (approved by the owner): signal frame on the guest stack
- Where: src/imports/signals.rs run_handler: frame = (interrupted sp - 872) & !7 instead of heap().malloc;
  guest sp = frame while the handler runs (call_guest starts it 256 bytes lower), interrupted sp restored
  after; heap free removed. tests/guest/selftest.cpp: on_pause checks that ucontext and siginfo lie between
  the handler's own frame and the interrupted sp; new check "signals: saved registers sit on the thread's
  own stack (where a garbage collector looks)".
- Predicted: the new check FAILS on the old build and passes on the new one; Card Wars 20 launches x 40 s
  with 0 crashes (before: 3 of 14; chance of 0/20 by luck at a 20% rate is ~1%).
- PC result: old PC binary + new self-test: "FAIL signals: saved registers sit on the thread's own stack",
  1 failure. New PC build: 0 failures in default and ARM32_FORCE_FALLBACK_SYNC=1.
- Phone result: (running: six modes, 20-launch loop, fps x2, Puzzle Craft 1 + 2)
- Review (Claude Sonnet 5.5 high, read-only, t1): no HIGH/MEDIUM. Frame lands below the interrupted sp in
  every path (call_stub, safe_point, helper deliver_for with parked sp, Unicorn call_guest_with, nested
  handlers, unicorn_live fallback); state restore correct; GC range [handler sp, stack_end] now covers the
  frame. LOW notes: pthread-specific values live host-side (Identity.values), invisible to a GC (not
  traced; Mono keeps thread objects in GC roots too); per-level stack use grows 872 bytes (old 256-byte gap
  already had the same edge risk); test covers only the self-delivery path and checks position, not contents
  (a stronger test would plant a value in a scratch register and find it on the stack). Not acted on: outside
  the approved scope; noted here.
- Phone result (fix 1 build): six modes 0 failures, new check ok in all: default 18 s, forced 14 s,
  CHECK=all 197 s (1369944 blocks, 0 differences), LSE=0 17 s, PARALLEL=0 54 s, unicorn 25 s.

## Research: does Mono rewrite literal words next to its JIT code? (suspect 2)
- Unity's Mono, mono/mini/tramp-arm.c mono_arch_patch_callsite (unity-main): for a `bx ip` call site it
  calls arm_patch(code) then `mono_arch_flush_icache ((guint8*)(code - 2), 4)`; mini-arm.c arm_patch_general
  "patching from magic trampoline" for the sequence
      ldr ip, [pc, #0] / b 0xc / .word code_ptr / mov lr, pc / bx ip
  writes only `code32 [-2] = target` (the .word). So after Mono resolves a call it rewrites the literal and
  flushes exactly those 4 bytes.
- In the bridge: the block starting at the `ldr ip, [pc, #0]` ends at the `b` (block [A, A+8)); the literal
  is at A+8, so invalidate()'s overlap test (block_end > start: A+8 > A+8 is false) keeps the block, and its
  folded literal still holds the old target (Mono's resolver trampoline). Every later call through that site
  goes through the resolver again (C code + locks) and re-patches + flushes again, forever. So suspect 2 is
  real, but it is a SPEED bug (calls keep taking Mono's slow path), not a likely crash cause.
- Thunks (emit_thunk: ldr ip,[pc,#0]; bx ip; .word target) flush all 12 bytes, so their blocks are dropped
  correctly.
- Card Wars 20-launch loop with fix 1 (x 40 s): 19 alive, 1 dead (run 6: `[fault] bad guest memory access at
  0x0000003b in translated code (guest block 0x025f63b0)`, r0=0xb, during LOAD_DATA right after
  "CardDataScript DeckData path"). Fix 1 did NOT remove the crash.

## Sub-agent critique (Codex Sol 6.1 high, read-only, t2) of the crash theory
- Statistics: 3/14 before vs 1/13 (then 1/20) after is not significant (one-sided p ~0.33 at 1/13). Keep fix 1
  (it is Linux behaviour), but it is not shown to help.
- The fixed-build fault at 0x025f63b0 = libmonobdwgc offset 0x003d73b0 = mono_string_is_interned_lookup
  (reads [r0+0x30] at 0x3d73c4 with r0=0xb): damaged object metadata / wrong pointer chain, not specifically a
  premature collection.
- Ruled out from the shipped library: no dl_iterate_phdr import, GC_register_data_segments is `bx lr` (Unity
  Mono registers its own roots); GC_dirty_init sets no page protection (no-op mprotect does not matter);
  stack bounds come from pthread_self -> pthread_getattr_np (self-query path is right).
- Strongest untested GC-contract issue: a signal helper (signals.rs:121, runtime.rs:609) acknowledges a
  SIGPWR pause for a thread that is still inside a host call, which keeps running; host memcpy/memmove/memset
  (libc_calls.rs:476) can write guest memory after the GC thinks the thread stopped. Against it: the fixed-
  build crash run had only one helper borrow, ~5 s before the crash, target likely in a futex syscall.
- Other real gaps (weaker matches): outer CPU state/parked registers saved only in Rust during guest
  callbacks (runtime.rs:1770/1796); helper's pthread_getattr_np returns the helper's own stack, other-thread
  query returns base 0; VFP registers not in the signal frame; new threads don't inherit the signal mask;
  gettid on a helper returns the helper's tid; sigaltstack fake; MADV_DONTNEED on unknown ranges silently
  not cleared; partial munmap keeps memory; membarrier failure ignored.
- Folded literals: confirmed defect; not only call targets: other literals may hold mutable pointers.
- Recommended order: (1) checker (check=1) through real loading; (2) parallel=0 runs long enough to pass
  loading; (3) trace helper borrows during loading (parked function, memory writes); then decide.
  Note: check=1 also turns parallel and linking off, so "checker passes and game lives" cannot separate a
  concurrency bug from a translator bug.
- Card Wars fps runs with fix 1: both died during loading. fps1: `[fault] bad guest memory access at
  0x00000010 (guest block 0x027072c8)` on worker arm32-guest-109 = libmonobdwgc +0x4E82C8, nearest symbol
  GC_clear_fl_marks (Boehm walking a free list: a free-list link was overwritten = classic sign of an object
  freed while still in use and then written). fps2: SIGSEGV in translated code on UnityMain at guest 0x10,
  abort message `* Assertion at boehm-gc.c:1433, condition GC_base (obj) == (char*)obj - offset` (pointer not
  at the start of a GC object). The bridge's own lines were lost: logcat ring buffer overflowed (thousands of
  [signal] lines); scripts now stream logcat live.
- Fix 1 build total: 3 dead of 22 launches (~14%), vs 3/14 before. No measurable change.
- Helper theory checked against data: the crashed runs loop-par/3, loop-par/5 and fix1-fps1 show
  "0 signal helpers" in their last [fps] line before the crash (fix1-loop/6 had 1, ~5 s earlier). Alive runs
  had 2-3. => helper borrows during host memcpy cannot explain these crashes. Ruled out for now.
- GC_get_main_stack_base (libmonobdwgc +0x4fb5e8) uses pthread_self -> pthread_getattr_np ->
  pthread_attr_getstack (self query, correct in the bridge). Ruled out.
- Puzzle Craft with fix 1: PC1 119.9 fps avg (min 117.9), PC2 119.9 (min 118.2), save files unchanged.
- fps2 detail: guest thread 0x1094 called abort() (boehm-gc.c:1433 assert), then UnityMain faulted in block
  0x02707178 (same function area as fps1's 0x027072c8 = GC_clear_fl_marks, same r1=146d7000/146d8000 r2=0x43):
  two crashes at the same Boehm free-list walk.
- Checker run (round2, fix-1 build, check=1, 300 s; note check=1 also turns parallel and linking off):
  predicted: a "difference in block" line = translator bug found; 0 differences through loading = translator
  bug in checked code unlikely. Actual: `[check] 2359296 blocks checked, 0 differences, not compared: 0
  (Unicorn stopped), 72 (Unicorn ran past the block)`. Mono init and the first scene load ran under the
  checker (Mono "Locale en-US" at +78 s, Unity "OnLevelWasLoaded" at +207 s, first [fps] line at +240 s,
  0.2 then 0.6 fps), game still alive when logging stopped at +300 s, no fault, no assert.
  Conclusion: no translator difference in 2.36 M checked blocks through the crash window; one alive run is
  not proof of no crash (14% rate), and parallel was off, so this does not separate the two theories.
- Crash timing (all 5 crashes so far): 11-16 s after the bridge starts, during loading. 3 of 5 right after
  `PATCHING ==> LOAD_DATA` while it reads Blueprints/db_Characters.json / db_Decks.json (heavy allocation,
  many GC pauses), 1 at WAITING_FOR_USERID. Parallel on: LOAD_DATA at +10 s, READY at +19 s.
- Parallel-off loop (fix1-nopar, 150 s each): loading ~7x slower (LOAD_DATA at +30 s, READY at ~+140 s or
  later), but every run passed LOAD_DATA and reached WAITING_FOR_USERID or READY, so unlike the earlier 40 s
  loop these runs do cover the crash window and count as evidence.
- fix1-nopar result: 6 alive, 0 dead of 6 (all passed LOAD_DATA). At a 14% crash rate, 6 clean runs happen ~40% of the time by chance: not decisive yet. Next: 14 more 100 s parallel-off launches (fix1-nopar2) to reach 20 (chance of 20 clean at 14% ~5%).
- Sub-agent review (2026-10-13, Sol 6.1 high, read-only), hunting parallel-only races. No proven cause.
  Found sound: enter/leave ordering, helper ownership, jump-table entries (one aligned atomic word), semaphore
  ordering, no double handler. In the 5 crash logs all 5,720 completed SIGPWR handlers ran at depth 1 (none
  inside a nested guest callback). Ranked gaps: (1) outer registers during guest callbacks kept only in Rust
  (runtime.rs:1770/1796/1804; callers qsort/bsearch/pthread_once/Java re-entry), moderate fit, weakened by
  depth-1 data; (2) VFP/NEON registers missing from the signal frame (signals.rs:332), low-moderate;
  (3) mprotect is a no-op (libc_calls.rs:1649) but libmonobdwgc's mono_memory_barrier_process_wide
  (+0x4bc888) uses the mprotect trick to force a barrier on all cores; only direct caller found is managed
  Interlocked.MemoryBarrierProcessWide, use during loading unknown; (4) helper acks while host writes go on,
  poor fit (0 helpers); (5) CAS STREX ABA, no Boehm/Mono site found relying on it; (6) code published even if
  the instruction-sync call failed (engine.rs:343/350). Also: new threads start with an empty signal mask
  (threads.rs:85). Suggested checks: count mprotect calls during loading; parallel with link=0; fault-time
  helper count and SIGPWR-entry callback depth.
- fix1-nopar2 (parallel off, 14 x 100 s): 14 alive, 0 dead; all 14 passed LOAD_DATA and reached
  WAITING_FOR_USERID. Total parallel off: 20 alive of 20. With parallel on: 3 dead of 22 (fix-1 build) and
  3 of 14 before. If parallel-off had the same ~14% rate, 20 clean runs would happen ~5% of the time.
  Conclusion: the crash needs parallel guest threads (or the different timing they bring). Translator
  instruction bugs and single-thread bridge bugs are now unlikely. Next: parallel on, block linking off.
- Checked (static): this Boehm build has no parallel marker threads (no GC_MARKERS string) and finds each
  thread's free lists through pthread_getspecific (imports pthread_key_create/get/setspecific). Bridge:
  values live per guest Identity, start at 0 for every new thread, handles are never reused
  (threads.rs:59-147). A new thread cannot inherit an old thread's free lists. Ruled out.
- Linking-off loop, first try (parallel on, link=0, 45 s): stopped after 3 runs: with linking off, loading is
  ~5x slower (first WAITING_FOR_USERID at +44 s vs +8 s), so 45 s never reached LOAD_DATA. Not evidence.
  Restarted as 15 x 110 s (fix1-nolink). Caveat: slower runs also mean fewer chances for a race, so a
  clean result would be weak evidence unless crashes still show up.
- fix1-nolink (parallel on, link=0, 15 x 110 s): 15 alive, 0 dead. Every run entered LOAD_DATA (~+83 s) but
  none finished it before the 110 s cut (most parallel-on crashes came within ~1.5 s of LOAD_DATA start,
  i.e. ~5-10 s here). At 14%, 15 clean runs happen ~10% of the time by chance. Suggests block linking (or
  the speed it brings) is part of the crash, but slowness alone could also hide a race. Next: one-core
  pinned loop (parallel and linking on, all threads on cpu 6) to separate "true simultaneous execution"
  from "bridge thread-switching logic".
- One-core loop (fix1-onecore, parallel and linking on, every game thread pinned to cpu 6 with taskset -a
  right after launch; checked: all 74 threads "Cpus_allowed_list: 6"). Loading timing stays close to normal
  (LOAD_DATA +15 s, READY +28 s vs +10/+19 s), so unlike parallel-off and link-off this keeps the timing.
  Predicted: crashes still at ~14% = a bridge logic race in thread switching/signals (interleaving only);
  0 of 20 = needs two cores at once (memory ordering, cross-core code/patch visibility, true data races).
- Found (static): the old ARMv6 CP15 barriers (MCR p15,0,Rt,c7,c10,5 = DMB; c7,c10,4 = DSB) are not
  translated (arm.rs:616 arm_coprocessor handles only TPIDRURO; thumb.rs:1707 the same) and go to Unicorn,
  whose helper.c:717-720 marks them ARM_CP_NOP: no host barrier at all. Harmless with one core, a real
  ordering gap with parallel threads on several cores. libmonobdwgc has 343 `dmb ish`, 4 `dmb sy`, and 1
  CP15 DMB; Mono's JIT (mini-arm.c:4259) emits `dmb ish` when v7_supported (forced TRUE when built
  HAVE_ARMV7), else the CP15 form. Whether the CP15 one runs during loading is unknown: the stats run
  (fallback sites) will tell. General engine gap either way; candidate fix: translate CP15 DMB/DSB/ISB to
  the host barriers.
- fix1-onecore: 20 alive, 0 dead of 20, all 20 reached READY (timing near normal). At 14%, 20 clean runs
  happen ~5% by chance. Conclusion so far: the crash needs guest threads running on two cores at the same
  moment (memory ordering / cross-core visibility / a true simultaneous race), not just the bridge's
  thread-switching logic. Caveat: base rate measured earlier in the day; a same-day control loop (normal
  settings, 20 x 45 s) runs next to make sure the crash still shows up.
- Checked (static) for two-core-only gaps, ruled out: guest mutex lock/unlock use Acquire/Release
  (threads.rs:165-225), pthread_once Acquire/Release, semaphores Acquire; kuser helpers have dmb ish
  (mem.rs:90-126); exclusive monitor address AND value are passed both ways between translator and Unicorn
  (engine.rs:677-745); DMB/DSB translate to host dmb/dsb ish (thumb.rs:1885). Only open two-core gap found
  so far: CP15 barriers -> Unicorn NOP.
- Control loop (fix1-control, normal settings) run 1: DEAD at +13 s, 0.9 s after "DeckData path:
  Blueprints/db_Decks.json" (LOAD_DATA). UnityMain faulted at guest 0x3b in block 0x025f63b0 (same block as
  an earlier crash, mono_string_is_interned_lookup area; r0=0xb, garbage pointer), and in the same
  millisecond thread 15598 hit `Assertion: should not be reached at class-accessors.c:86` (bad MonoClass
  kind). Two threads reading the same corrupted Mono metadata at once. Crash still present today.
- Control run 6 also DEAD, same block 0x025f63b0, r1 same. Disassembly (libmonobdwgc +0x3d73b0, inside
  mono_string_is_interned_lookup, built -O0): r0 = str->vtable; ldr [vtable] (klass); ldr [klass+8];
  ldr [+0x30] faults at 0x3b/0x45, so klass+8 held 0xb/0x15. The string being interned has a garbage
  vtable chain: the look of a GC free-list link in the object's first word, i.e. the string object was
  freed by Boehm while UnityMain still used it (a missed root / missed mark), then reused. Matches the
  other crash kinds (free-list walk faults, GC_base assert, bad MonoClass asserts).
- Next decisive split (needs a small code change, asked the user): run with Boehm collection turned off
  (GC_DONT_GC=1, read by Boehm via getenv, which the bridge passes to the host getenv). If crashes vanish,
  the GC is freeing live objects (root/marking problem on two cores); if they stay, it is a plain data
  race / ordering problem in other code. The app process env can't be set from outside on a user build,
  so this needs a general debug property (debug.arm32bridge.env) that the bridge applies with setenv at start.
- Control loop result (fix1-control, normal settings, 20 x 45 s): 17 alive, 3 dead (runs 1, 6, 14), all
  in block 0x025f63b0 (fault 0x3b/0x45/0x3b). Same-day rate ~15%, matches the earlier 3/22. So the
  parallel-off 20/20, link-off 15/15 and one-core 20/20 results stand against a live base rate
  (chance of 20/20 at 15%: ~4%).
- The stats run after it (debug.arm32bridge.stats=1, 100 s) also died in loading: fault at 0x10 in block
  0x02707178 (r0=0), a different spot - 4 deaths in the last 21 normal launches.
- Fallback sites from that stats run (37 sites, top: Thumb f383 0205 x1072 (MSR APSR_g), ARM f2e10070,
  Thumb efe1 8070/2072 (NEON), e6e2/e6e1 0010 (USAT) x80 each, e89cffff (LDM with pc)): no CP15
  barrier (ee070fba) anywhere. Mono's JIT detects v7 and emits `dmb ish`. So CP15 barrier -> Unicorn
  NOP is NOT what causes this crash. The only CP15 DMB in libmonobdwgc is inside
  mono_arch_output_basic_block (JIT emitter path for pre-v7 CPUs).
- Fixed anyway (general gap, tiny; owner asked 2026-10-13 to fix obvious problems at once): the
  translator now turns the ARMv6 CP15 barriers (MCR p15,0,Rt,c7,c10,4/5 and c7,c5,4) into host
  dsb ish / dmb ish / isb in ARM and Thumb (thumb.rs cp15_barrier, used by arm.rs arm_coprocessor and
  thumb.rs coprocessor) instead of leaving them to Unicorn, where QEMU treats them as no-ops. Self-test:
  the SWP spinlock now uses CP15 DMB for its acquire/release, and every race adder runs all three CP15
  barriers in its own mode.
- Ruled out by reading: VFP registers missing from the signal frame. Signals are only delivered while a
  thread is parked in a phone function (a call boundary); the handler is an AAPCS function that keeps
  d8-d15, and d0-d7/d16-d31 are dead at a call. Not a real gap.
- mono_memory_barrier_process_wide (mprotect trick; guest mprotect is a no-op) is only reached from the
  C# icall Interlocked.MemoryBarrierProcessWide: unlikely to matter here; left alone.
- Test switch added (owner approved, 10 launches): debug.arm32bridge.env="NAME=VALUE;..." is applied once
  with setenv at the first bridge load (app.rs set_debug_environment), before any game code. Off unless
  set. Boehm in libmonobdwgc reads GC_DONT_GC (string present).
- GC split result (gcoff, debug.arm32bridge.env=GC_DONT_GC=1, 10 x 45 s; "debug environment GC_DONT_GC=1" in
  all 10 logs; fps lines show 0 signal helpers, i.e. no GC pauses at all): 7 alive, 3 dead. Run 1 alive but
  never READY after "Assertion: should not be reached at class-accessors.c:184"; run 2 fault at 0x0 in block
  0x026ccd20; run 8 fault 0x3e in block 0x025f63b0 (the same mono_string_is_interned_lookup spot, r1 same);
  run 10 fault 0x3c in block 0x02504578. Same rate and same kinds as with the GC on (3/20 + 1).
  Conclusion: the collector freeing live objects is RULED OUT. With nothing ever freed, Mono still sees
  garbage class/vtable chains: a real two-core race (memory ordering / missed exclusion / wrong
  publication) in bridge or translator behavior. Also ruled out by reading: pthread key values (per
  Identity, per engine), mutex word layout (owner id << 16, small sequential ids), stack/TLS block reuse
  (handed back only in drop_engine, after the thread's last guest code).
- Phone self-test after the CP15 change: 0 failures in five modes; ARM32_CHECK=all had 1 failure,
  "futex wake counts" (first WAKE 2 reported 1): the test sleeps 50 ms expecting three threads asleep,
  and under the checker (every block runs twice, 183 s run) they were not yet asleep. Test timing, not an
  engine bug: the wait is now 1 s when the checker is on.
- Sub-agent review started (Sonnet 5.5 high, read-only, t4): attack the race theory, list ordering gaps.

### Speed: lock-free dispatcher lookup (owner approved)
- Profile prof2g (dwarf, 6 s, steady state, before the change): UnityMain spends 25% of its samples in
  engine::run itself (whole process: 20%). Instruction histogram (perf-ips.py): 813 samples on the
  HashMap bucket key load (cache miss in a 186k-block table), 613 + 494 + 199 on the RwLock read lock /
  unlock atomics (the lock's cache line bounces between cores), plus a jump-table store on every lookup
  (set_jump in block_for's hit path, also a shared line). ~2000 of 2644 run() samples.
- Change (engine.rs): run() first reads the jump-table entry for pc (one Acquire load, no lock); a match
  gives the block (code = CODE_BASE + offset; enter glue and exit stub from new statics ENTER/EXIT_STUB).
  Only on a miss does it take the RwLock path (block_for), which refills the entry. set_jump now stores
  only when the entry changes. Safe because blocks are dropped only in invalidate() under stop_world,
  which clears their entries; the table is only used when linking is on (checker turns linking off, and
  it is the only user of the block end, which the fast path returns as 0).
- Expected: run() self time down by most of the ~75% lock/hash share, main thread ~15% less work; fps up
  from ~72 if the main thread is the limit. Measuring: 3 x 120 s fps runs + crash loop + all checks.
- Second speed change, tested together with the lookup change (the first check round was stopped
  because its phone build may have caught this edit half-done): the slow per-thread values (Rust
  thread_local! on Android is pthread-key based, `std::sys::thread_local::os::Storage::get`, 8.7% of
  UnityMain samples, half from call_stub). call_stub used 5 of them per phone call (RESUME_AT x2,
  PHONE_DEPTH x2, PHONE_TICKS); they are now fields of the thread's engine data (Thread.resume_at,
  phone_depth, phone_ticks; resume_point(uc)). The bridge-lock counter LOCKS_HELD (2 per shared()/heap()
  lock, ~1.6M heap calls per 100 s) is now kept only when lock checking is on (debug builds,
  ARM32_LOCKCHECK=1), which is the only reader. No behavior change. fps result is the combined effect.

### Review t4 (Sonnet 5.5 high, read-only) and the ordering split
- Corrections it made, checked: gcoff runs 1 and 2 were out-of-memory deaths (no collection, so memory
  runs out: 101 OutOfMemoryException lines and refused frees in run 2, low-memory-killer in run 1), so
  only runs 8 and 10 are real crashes (0 OOM lines, 0 refused frees). And GC pauses were not zero: 15
  SIGPWR rounds per run (vs ~1000-2400 normally); the GC is far less involved but not absent.
  Revised: GC-off 2 real crashes / 8 usable runs, with ~100x fewer pauses. Freeing live objects is
  impossible there (nothing is freed), so that theory stays ruled out; pause handling stays unlikely.
- Its main point: all three safe settings also add strong ordering. Link-off sends every block boundary
  through block_for's RwLock (an acquire + release RMW on one shared word, close to a full fence).
  So link-off safe does not point at link patching; it fits "ordering at block boundaries" as well.
  (Note: the new lock-free dispatcher removes that lock from the dispatcher path too.)
- Other candidates it ranked: stale folded literals (literal at exactly a block's end is outside the
  invalidate range, thumb.rs:536-545 / engine.rs invalidate); CAS-emulated exclusives (ABA, wider
  window); latent game race exposed by sysconf reporting all host CPUs. Cleared: stop_world /
  enter_guest Dekker pairing, link bookkeeping, encodings, pthread code, made_blocks range maths.
  Smaller defects it found (not the cause): GuestHeap::not_owned may refuse frees of big mapped
  chunks (mem.rs:296); normal-mutex unlock by a non-owner returns EPERM; partial munmap is a no-op.
- My doubt about its top theory: the same Mono/Unity code runs natively in AArch32 on out-of-order
  ARMv8 cores (A76/A78 phones) without crashing, and the translator keeps loads/stores 1:1 with their
  barriers, so a pure "host reorders more" story needs something the translator changes. Still, a
  block-start barrier is the cheapest decisive split.
- Experiment switch added (off by default): debug.arm32bridge.fence=1 / ARM32_FENCE=1 puts dmb ish at
  the start of every translated block (thumb.rs fence_blocks, used by both translate functions), so
  linked jumps order memory like the lock path did. Prediction: if ordering at block boundaries is
  the cause, 0 of 20 die; if 2+ of 20 die, it is not.
- Phone cores: cpu0-1 Cortex-A520 (in-order, midr d801), cpu2-6 A725 (d871), cpu7 X4 (d821).

### Fix 2: literals in game-made code are read at run time
- Confirmed by reading (review t4 and thumb.rs load_store): every word LDR with a pc base was folded
  into a constant at translation time. Mono's JIT patches literal words in place (call sites like
  `ldr pc, [pc, #-4]; .word target`, constants) and flushes only those 4 bytes. A literal sitting at
  or after a block's end is outside the block's range, so invalidate keeps the block and the
  translated code keeps the OLD value (for a call site: the trampoline, every time).
- Change (thumb.rs load_store, shared by ARM): fold only when the literal address is inside library
  code (LIB_BASE..LIB_END, which never changes); otherwise emit a real load (and for rt = pc, a real
  load into T6 then the normal computed jump, which the old non-folded path never handled).
  VLDR literal and LDRD literal were already real loads.
- Self-test added (all modes): made code with `ldr r0,[pc,#0]; bx lr; .word` and
  `ldr pc,[pc,#-4]; .word target`, each literal patched and flushed alone; the new value must be seen.
  Expected: the build before the fix FAILS it on the translator; the new build passes.
- Note: the speed round's Puzzle Craft step (job 011) rebuilds the bridge library, so it ran with this
  change (and the fence switch, off) included; cocos2d-x makes no code at run time, so it is unaffected.
- PC self-test can't show the literal bug: on the PC there is no translator (it emits AArch64), so
  everything runs in Unicorn, which reads literals at run time. The old PC build passed the new test,
  as it must. The before/after check runs on the phone (old phone binary + new self-test first).
- Speed round results (lock-free lookup + per-thread values; fps from [fps] lines, steady part, 120 s
  runs): run 1 75.8 fps (game 13.22 ms, own 8.72, phone 4.49); run 2 died in loading (fault 0x34 in
  block 0x02504578, a known crash kind); run 3 76.4 fps (game 13.11, own 8.57, phone 4.54).
  Before (this session's runs): ~72 fps, game 13.4-13.6, own ~9.25, phone ~4.2. Gain ~+4 fps,
  own code -0.6 ms per frame: smaller than the profile suggested (run() was 25% of UnityMain).
  Next: profile again to see where run() time went.
- Speed round crash loop (speed2-loop) INVALID from run 5: runs 3 and 4 died back to back (Mono
  asserts class.c:2214 and handle.h:681 m_class_is_valuetype), Android then showed its "Application
  Error: com.ThugBoatLTD.CardWars" box, which kept focus (mCurrentFocus = Application Error window), so
  later launches never got window focus: UnityMain idle in epoll, no guest threads, no [fps]. Runs 5-7
  "alive ready=0" are that, not freezes. Usable: runs 1-4, 2 dead. Stopped the round; asked the owner
  how to clear the box (screen action). Test scripts must check mCurrentFocus before trusting a run.
- Literal fix confirmed on the phone: OLD phone build + new self-test -> "FAIL made code: a literal
  patched after the block's end (constant, call target) is seen" (1 failure); new phone build -> ok,
  0 failures (translator and forced-fallback modes so far). So Mono-style 4-byte literal patches were
  really missed before.
- Crash-box handling (owner chose "turn off Android's crash box during tests"): originals were unset;
  set global hide_error_dialogs=1 (the setting that also stops the repeat-crash box; the first-crash
  setting alone does not) and show_first_crash_dialog=0; restore = delete both
  (/tmp/a32k/crash-dialogs.sh restore). cw-crashloop.sh now records mCurrentFocus after each run and
  marks a run INVALID(error box) if an Android error/ANR box had focus.

### Review t7 (Sol 6.1 high, read-only), early findings, fixed at once (2026-10-13)
- **Frame timer took the engine before the guest lock.** `app.rs` read `phone_time()` (now `engine().get_data().phone_ticks`) before `hold_gil()`. With parallel mode off, Unicorn mode or the checker on, `engine()` asserts "engine used without the guest lock". The self-test never goes through this Java entry path, so all six modes still passed. Fix: read `phone_before` right after `engine()`. Ticks only change inside guest calls, so reading it after the lock wait gives the same number. `cargo check` clean (job 014).
- **Conditional ARM CP15 barriers fell back to Unicorn.** `cp15_barrier()` masks with 0xFFFF_0FFF, so the ARM condition nibble had to be E. `arm_instruction` has already emitted the condition skip before `arm_body`, so `arm_coprocessor` now matches `(word & 0x0FFF_FFFF) | 0xE000_0000`. Thumb is unchanged (IT blocks are handled outside).

### Review t7 (Sol 6.1 high), final findings and what was done (2026-10-13)
- **P1, two live `&mut Uc` to one engine.** `phone_time()` and `share_gil()` called `engine()` while the caller still held its own `uc`. Fix: `phone_time(uc: &Uc)` and `share_gil(uc, resume)`, which uses `deliver_pending_on(uc)`. Both call sites already had `uc`. `cargo check` clean (job 015).
- **P2, the library range admitted writable data.** Literal folding and "never watch for new code" applied to all of `LIB_BASE..LIB_END`, which includes each library's .data and .bss. Fix: the ELF loader records the PT_LOAD segments without PF_W (`mem::add_fixed`). `mem::fixed(addr)` is now used for literal folding (both ends of the word), for skipping `made_blocks` in engine.rs, and for the mprotect "code made while running" log. The old `mapped_by_library` helper is removed. All three games keep code in R E segments (readelf: libunity, libmonobdwgc, libmono-native, libmain, libMonoPosixHelper, libcocos2dcpp), so their speed should not change. The self-test now also runs the patched-literal check on a page-aligned .bss buffer made RWX with mprotect. Known limit: a library whose *code* is later made writable by guest mprotect is still treated as fixed (no game seen doing that).
- **P2, sensor callbacks dropped.** Not changed. The bridge has no looper-callback support at all (no ALooper_addFd), so this is a wider gap. Returning NULL could crash a game that runs fine without sensor data today. createEventQueue logs when a callback is passed, so a game that needs it will show up in the log.

### Per-thread guest malloc cache (sub-agent t6, Opus 5.5 high) — kept aside, not merged (2026-10-13)
- What: each engine keeps small freed blocks (chunks ≤ 1024 B) so malloc/calloc/free mostly skip the HEAP mutex; not_owned made lock-free; double-free mark in freed payload. Worktree ~/dev/arm32-bridge-malloc (branch agent/malloc), own diff /tmp/a32k/malloc.diff.
- PC results (agent): self-test 0 failures in 3 modes; cargo test 6/6 (release + debug); 4 real host threads × 200k pairs: heap lock 128 ms → caches 2.1 ms; guest self-test 66 → 61 ms (PC Unicorn runs one guest thread at a time).
- Its "not_owned refuses mapped chunks" suspicion is wrong for dlmalloc 0.2.14 (no mmap path; checked by review t8 too).
- Review t8 (Sol 6.1 high): P1 lock-free header reads race with dlmalloc's plain writes to neighbour headers (formal UB). P2 engines of Java/audio host threads are never dropped, so their caches pin up to ~440 KiB each forever. P2 double-free swap can accept a block after the winning cache drained (corrupts dlmalloc free list). P2 HEAP during realloc does not exclude a concurrent cached free. (P2s 3-4 need invalid guest behaviour.)
- Decision: not merged. Profiles say the heap is not a hot spot for Card Wars: today's UnityMain profile (prof2g) has no dlmalloc/heap entry in its top list; perf7 had Mutex::lock_contended 0.43% for everything. ~1.6M heap calls / 100 s ≈ 130 per frame × ~0.3 µs ≈ 40 µs of a 13 ms frame. Best case < 1% fps, against 4 open issues. Revisit only if a profile shows heap time.
- Profile lead noted while checking: linux_syscall → wake_on_count 5.2% (Vec::retain 3.9% inside, futex_wake 4.0%) on the game thread. Candidate for the next speed step.
- Checked the lead right away: `Vec::retain` has only 0.17% self time. Its 3.75% children are `Parker::wake` → Condvar notify → kernel futex_wake / try_to_wake_up (`_raw_spin_unlock_irqrestore` 3.57%). That is the cost of really waking a sleeping thread, which native code pays for its own futex wake too. The waiter scan itself is cheap, so there is nothing to gain here. Not pursued.

### Round 13 barrier split, partial (2026-10-13), and its speed cost
- Blocks of 10 Card Wars launches x 45 s, debug.arm32bridge.fence on/off alternating; literal fix in.
- fence-on1: 10 alive, 0 dead. fence-off1: 8 alive, 2 dead (runs 3, 9: fault 0x30 / 0x3b in block 0x025f63b0, the usual spot). fence-on2: 7 so far, 0 dead.
- Speed, last 10 [fps] lines of each run (fps / game ms / own ms): fence on ≈ 61 / 16.2 / 12.1; fence off ≈ 76 / 13.2 / 8.6. A dmb at every block start costs ~40% of own-code time. Not acceptable as a fix; it only locates the problem.
- Checked by reading while it ran: the bridge's pthread mutex/rwlock/sem (threads.rs) use Acquire CAS to lock and Release CAS to unlock; kuser helpers have dmb; the Unicorn fallback runs single unsupported instructions (no guest barrier lost). Exclusives are QEMU-style (LDREX records value, STREX = CASAL against it; ABA passes). Not yet tied to the crash.
- Asked a fresh critical sub-agent (t10, Sol 6.1 high) to attack the ordering theory and propose split experiments.

### Round 13 result (job 013, 2026-10-13)
- Old phone build + new self-test: `FAIL made code: a literal patched after the block's end` (1 failure). New build: 0 failures in all six modes (translator, forced fallback, ARM32_CHECK=all [1415994 blocks, 0 differences], LSE=0, PARALLEL=0, Unicorn). Literal fix confirmed on the phone.
- Barrier split, 10 launches x 45 s per block: on1 0/10 dead, off1 2/10 (0x30 / 0x3b in block 0x025f63b0), on2 0/10, off2 2/10 (0x10 in blocks 0x02707178 / 0x027072c8, r1=146d7000 both: libmonobdwgc free-list walk area). Total: fence on 0/20, fence off 4/20. One-sided Fisher p = C(20,4)/C(40,4) ≈ 0.053.
- Puzzle Craft (build from the tree at that time, with review fixes): pc1 119.6 avg / 116.2 min fps, pc2 119.8 / 116.6, save files unchanged.

### Critique t10 (Sol 6.1 high, read-only) of the ordering theory
- The fence result shows "something changes when cores overlap", not specifically missing ordering. Per-block dmb also slows guest work ~40% and moves code, so timing could explain it too. Asked for split experiments.
- Proven gap: LDRD with rt == base emitted two separate `ldr w` (arm.rs ~448, thumb.rs ~1191), which can tear an 8-byte-aligned pair. Native LPAE cores read it in one go (QEMU also keeps one 64-bit access). Unicorn's own LDRD/STRD split too, so the checker cannot see this. FIXED at once: always one LDP (single-copy atomic as a whole with FEAT_LSE2, which the Pixel 10 cores have). Static count in the shipped libraries: libmonobdwgc 0 of 1 LDRD have rt == base, libunity 1 of 592, libcocos2dcpp 3 of 701. So this is unlikely to be the Card Wars crash (only Mono's JIT output is left unchecked).
- Jump-table read in generated code is a plain `ldr x` (thumb.rs emit_exit_to); the Rust dispatcher uses Acquire. A protocol mismatch, but code visibility comes from __clear_cache + membarrier SYNC_CORE, and an acquire does not order instruction fetch. Lower rank; kept for a later toggle.
- Exclusives: QEMU-style value compare (ABA, same-value stores unnoticed); only a specific algorithm (read head, read head->next, replace head) would break. ARM32_LSE=0 does not test faithful monitor semantics.
- Ruled lower by the reviewer: pointer dependencies (kept, including through register spills), conditional/IT branches, LDM/STM, VLDR/VSTR, barrier option mapping, safe points, code invalidation. Correction: signals can be delivered at block boundaries (safe_point), not only at calls.
- Its experiments: (1) same-size prefix with no ordering (nop) vs dmb; (2) LDRD fix (done); (3) ldar for the generated jump-table read; (4) dmb ishld vs dmb ishst.
- Done: the fence switch now has modes: debug.arm32bridge.fence = 1 (dmb ish), ld (dmb ishld), st (dmb ishst), nop.

### USAT / SSAT / USAT16 / SSAT16 translated (sub-agent t5, Opus 5.5 high), merged 2026-10-13
- Why: the stats run listed Thumb f383 0205 x1072 (earlier misread as MSR APSR_g; it is `usat r2,#5,r3`) and ARM e6e2/e6e1 0010 (USAT) x80 among the top Unicorn fallback sites.
- What: thumb.rs saturate / saturate_instruction / saturate_halves_instruction, decoded from Thumb plain-binary-immediate ops 10000/10010/11000/11010 and ARM arm_media (before PKH). Range test sbfx+eor (signed) or lsr (unsigned), cbz to keep the value, else eor/bic of the bound with x asr #31; Q (CPSR_OTHER bit 27) set only when saturating; NZCV untouched. Self-test: every width, LSL/ASR shifts (ARM ASR #32), Q sticky, NZCV unchanged, conditional ARM and Thumb IT forms, Rd = Rn.
- The agent could not compile in its own session. I built its worktree: PC build clean, PC self-test 0 failures in default and forced-fallback modes (the new check passes: Unicorn matches the C reference).
- Review t11 (GLM 5.3 Flash, read-only; t9 Gemini 3.8 Flash gave no output in 23 min and was stopped): no functional bugs. Encodings checked with clang; no decode overlap with PKH, parallel add/sub, SEL, SXTAH, REV/RBIT, SMUL/BFX/BFI. It modelled the emitted sequence in Python: 300k random + edge inputs, every width, 0 mismatches. Nit only: Thumb SP as Rd/Rn (UNPREDICTABLE) translated like the rest of the data-processing code.
- Merged into the main tree (/tmp/a32k/usat.diff). cargo check clean. Phone translator self-test still to run.
- Store-only block-start barrier (dmb ishst), cost from round 14 block 1 (last 10 [fps] lines per run): ≈ 73 fps / 13.7 ms game / 9.1 ms own, against ≈ 75 / 13.4 / 8.7 with no barrier (round 13 off blocks). About 5% of own code, against ~40% for the full dmb.
- Ruled out by reading: Mono's JIT emitting no barriers because it thinks the CPU is pre-v6 (OP_MEMORY_BARRIER emits nothing when neither v7 nor v6 is detected). libmonobdwgc's mono-hwcap-arm.c strings: "!uname (&name)", "armv8", "/proc/cpuinfo", "model name", "(v", "Features". uname reports the host machine (aarch64/armv8 counts as v7), and the bridge's fake /proc/cpuinfo says "model name : ARMv7 Processor rev 0 (v7l)" plus "CPU architecture: 7". Both paths give v7, so the JIT emits dmb. Mono does not import getauxval (the bridge's getauxval returns 0).

### Round 14 (jobs 019 + 01A + 01B, 2026-10-13): fence modes on the build with review fixes + LDRD fix
- Build differs from round 13's by: review fixes (phone_time/share_gil take uc, fixed = read-only library segments for literal folding and made_blocks, conditional ARM CP15 barriers), the LDRD single-LDP fix, and the fence-mode code (no prefix when unset).
- PC check: new self-test fails on the old PC build only on the signal-frame test (as expected); new PC build 0 failures, default and forced fallback. Phone: six modes, 0 failures.
- Card Wars, 10 x 45 s per block (owner asked for 10 each; a watcher cut the planned second pass): fence=st 0/10, fence=ld 0/10, fence=nop 0/10 (counted from saved logs by fault/assert lines; the same count gives 2/2/0 for round 13's off1/off2/on1, so the method is right).
- 0/30 against the old ~20% rate (P ≈ 0.8^30 ≈ 0.001). Either a code change since round 13 fixed it, or any one-instruction block prefix (even a nop: layout/timing) hides it. Job 01B runs fence unset on the same install to split these.
- Static check while 01B runs: PC-relative loads (ldr/ldr.w/ldrd/ldrb/ldrh/... [pc]) whose target lies in a writable segment: libmonobdwgc 0 of 36574, libunity 0 of 4059, libmain 0 of 3, libmono-native 0 of 943. So the "fold only read-only segments" fix changes nothing for Card Wars' library code. The phone_time/share_gil fixes do nothing in parallel mode (share_gil returns early; phone_time only feeds the [fps] line), and none of the CP15 barriers are hit. If 01B is clean, the LDRD fix is the only code change left that touches Card Wars (1 rt==base LDRD in libunity, plus whatever Mono's JIT emits).
- 01B (fence unset, same round-14 install): 9 alive, 1 dead (run 10: fault 0x35 in block 0x025f63b0 + "Assertion: should not be reached at class-accessors.c:86"). The crash is still in today's code; no change since round 13 fixed it. Round 15 (rerun round-13 code from worktree agent/r13) was stopped before it built anything; the phone still has the round-14 install (lastUpdateTime 20:16:52).
- Totals on comparable builds: any one-instruction block prefix (1 / st / ld / nop) 0/50; no prefix 5/30. Prefix vs none: one-sided Fisher p = C(30,5)/C(80,5) ≈ 0.006. But nop alone is only 0/10 (P(0/10 | 1/6) ≈ 0.16), so "ordering" vs "layout/timing" is still open.
- Round 16 (job started 20:5x): six-mode self-test with USAT merged, then nop / off / nop blocks of 10 on the round-14 install.
- New working theory to test against the nop result: Mono/Unity may have a latent publication race (e.g. object fields vs the pointer to the object) that older AArch32-capable cores rarely expose. The X4/A725 reorder more aggressively and no longer run AArch32, so nobody has run this code natively on them. If true, the translator is not weakening anything; the bridge would have to give stronger ordering than ARM promises (TSO-like store ordering) to run such code safely.

### Round 16 (2026-10-13): nop vs off, same round-14 install
- Phone self-test (current main with USAT/SSAT, LDRD as one LDP, fence switch, read-only literal fold, library-data literal test): 0 failures in all six modes; ARM32_CHECK=all 1,685,652 blocks checked, 0 differences.
- Card Wars, 10 x 45 s per block: nop 2/10 dead (fault 0x10 block unknown; assert class.c:4575 m_class_get_rank), off 1/10 (fault 0x10 block 0x027072c8), nop 1/10 (fault 0x39 block 0x025f63b0).
- Prediction was: nop crashes like off -> ordering; nop clean -> layout/timing. Result: nop 3/20 here (3/30 with round 14), so the nop does nothing. ORDERING it is.
- Totals over rounds 13-16: real barrier prefix (dmb ish / ishst / ishld) 0/40; no barrier (none or nop) 9/70. P(0/40 at 9/70) ~ 0.4%.
- Static reading while it ran:
  - Block 0x027072c8 = libmonobdwgc +0x4e82c8 = GC_clear_fl_marks, at `hhdr = HDR(h)` for the next free-list block; the ldr [hhdr+0x10] (hb_sz) faults because HDR() is 0: a global free-list link points outside the GC heap. The faulting thread (UnityMain) is the collector, right after its own world restart (GC_finish_collection runs with the world running but under the allocation lock).
  - The 1-2 ms gap between a helper-run GC signal (thread 4244) and two crashes is NOT a lead: 4244 is thread 37, always asleep in nanosleep (2^24 ns = Boehm GC_lock back-off), so a helper serves it in every GC cycle.
  - Boehm here has no parallel marking (no "GC_MARKERS"/mark-helper strings) and no thread-local allocation (GC_malloc_kind -> GC_malloc_kind_global). Its allocation lock: AO_test_and_set = word `ldrex; strex` loop in one block, then `dmb sy`; unlock = `dmb sy; str #0`. LOCK() is guarded by GC_need_to_lock (Mono sets it via GC_allow_register_threads). Translated correctly as far as reading shows.
  - Code publication reread: code space is a bump allocator (never reused), sync_other_cores before linking, jump-table Release store. No gap seen.
  - Open: ARM conditional non-branch instructions are emitted as a branch around the body (arm.rs arm_instruction), which turns a flags/data dependency into a control dependency. Not yet shown to matter.
- Sub-agent t12 (Sol 6.1 high, read-only) started: hunt for any place translated code gives weaker ordering/atomicity than native AArch32.
- t12 (Sol 6.1 high) result: no confirmed cause behind Boehm's correctly fenced lock. Real gaps found, all judged low fit:
  1. Value-based exclusives allow ABA; safe-point parking (runtime.rs:738) keeps the record. Fits lock-free lists, not this lock; barrier prefixes would not repair it (so it does not fit the 0/40).
  2. Shift expansion drops dependencies: thumb.rs:279 LSR #32 -> constant 0, :350-352 register shifts >= 64 -> 0, :361 ASR amount >= 32 -> 32. ishld would close it, ishst not. No A32 `LSR #32` in libmonobdwgc code; Mono JIT output unchecked.
  3. Unicorn fallback still splits aligned LDRD/STRD (vendor translate.c:9174/9214); QEMU fixed this upstream. Reachable only via fallback paths (unsupported instruction in an IT block, arm.rs:431 register-offset overlap).
  4. mprotect does not update FIXED (libc_calls.rs:1681): a library segment made writable and patched keeps stale translations. No sign Card Wars does it.
  Also: the generated jump-table ldr has no acquire, but no stale-data failure found (no host literal pools, append-only code, cross-core sync before publication); unchecked sync syscall result at engine.rs:378.
  Its caution: ld 0/10 and st 0/10 do not prove both repair the same defect.
- Status: targeted cause not found after rounds 13-16 plus two read-only audits. Next step is a choice for the owner (speed cost vs crash fix).

# OPEN: root cause behind the light barrier (handoff, 2026-10-13)

The owner chose (2026-10-13) to ship a general workaround: a `dmb ishst` at the start of every translated
block in parallel mode (about 5% more own-code time). We may come back and find the real cause so the
barrier can go. Everything needed to restart is below; also read rounds 13-16 above.

## Symptom
- Card Wars (Unity + Mono 2.0 "bdwgc" build, ARM mode, -O0) dies in ~13% of 45 s launches, almost always
  while loading (ready=0). Puzzle Craft 1/2 (cocos2d-x) never showed it.
- Crash kinds (all look like the same corruption of allocator / Mono metadata memory):
  - fault at 0x10, block 0x027072c8 / 0x02707178 = libmonobdwgc +0x4e82c8 / +0x4e8178 = GC_clear_fl_marks:
    a GLOBAL free-list link points outside the GC heap, HDR() returns 0, `ldr [hhdr+0x10]` faults.
    Faulting thread = UnityMain = the thread that just ran the collection (GC_finish_collection, world
    already restarted, allocation lock held).
  - fault at 0x30-0x45, block 0x025f63b0 = libmonobdwgc +0x3d73b0, mono_string_is_interned_lookup:
    str->vtable->klass chain garbage ([klass+8] = 0xb/0x15, looks like a free-list link in the object's
    first word).
  - Mono asserts: class.c:4575 `m_class_get_rank (ac)`, class-accessors.c:86/184 (bad MonoClass kind),
    GC_base assert.
- Library base for offsets: libmonobdwgc loaded at 0x0221f000 in these runs (0x025f63b0 - 0x3d73b0).
  APK: ~/dev/arm32-bridge-games/cardwars/CardWars.apk; extracted lib in /tmp/a32k/cw (re-extract with unzip).

## What the barrier data says (10-launch blocks, 45 s each)
| prefix at every block start | dead / runs |
|---|---|
| none | 6/40 |
| nop (same code size, no ordering) | 3/30 |
| dmb ish (full) | 0/20 |
| dmb ishst | 0/10 (+ round 17, see below) |
| dmb ishld | 0/10 |
- Barrier 0/40 vs no barrier 9/70: p ~ 1.4% (Fisher exact; the first "0.4%" was a binomial shortcut, corrected by review t17). nop does nothing, so it is ORDERING, not code layout/timing.
- Caution (t12): ld 0/10 and st 0/10 each are weak; they may not repair the same defect.
- Needs two cores at once: all threads pinned to one cpu 0/20; parallel off (one guest lock) 0/20; block
  linking off 0/15. Same-day controls kept the ~15% rate, so those are real.
- GC turned off (debug.arm32bridge.env=GC_DONT_GC=1, verified 0 GC signals): still 3/10 dead with the same
  kinds. So the collector freeing live objects is RULED OUT; it looks like allocation handing out the same
  memory twice, a lost/torn update, or a stale read of allocator/metadata state.

## Ruled out (with how)
- Bridge mutex / rwlock / pthread_once / semaphores: Acquire/Release (threads.rs reading).
- kuser helpers: have dmb ish (mem.rs).
- Guest DMB/DSB/ISB, all options: become host dmb ish / dsb ish / isb (thumb.rs barrier()). ARMv6 CP15
  barriers now translated too (were Unicorn no-ops; not executed by Card Wars per stats run).
- Mono emitting no barriers: hwcap detection gives v7 (uname armv8, fake cpuinfo v7l) so the JIT emits dmb.
- Boehm build facts: no parallel marking (no GC_MARKERS / mark-helper strings), no thread-local allocation
  (GC_malloc_kind -> GC_malloc_kind_global). Allocation lock GC_allocate_lock (0x5c5dc4): AO_test_and_set =
  word `ldrex; strex` loop in one block (+0x5022e4), then `dmb sy` (+0x4df8b0); unlock = `dmb sy; str #0`
  (+0x4df8b8). LOCK() guarded by GC_need_to_lock (0x5c5dc0). GC_lock spins sched_yield x12, then nanosleep
  2^i ns up to 2^24 (~17 ms), which is what thread 37 shows in logs. Translation of all of this reads correct.
- Exclusive monitor address+value passed both ways between translator and Unicorn (engine.rs ~705).
- LDRD rt==base tearing: fixed (one LDP); libmono had 0 such sites, libunity 1.
- Literal folding from writable segments: fold now limited to non-PF_W segments; Card Wars had 0 of ~41k
  PC-relative loads targeting writable segments anyway.
- Futex wake cost/counting, heap (malloc) contention: not involved.
- Signal frames: frame on the guest stack with r0-r14 (signals.rs run_handler); helpers stand in for
  threads stuck in phone functions and the thread cannot re-enter guest code until the helper is done
  (enter_guest / borrowed_from). The "helper ran 1 ms before the crash" pattern is a coincidence: thread 37
  is always asleep in GC_lock's nanosleep, so a helper serves it in every GC cycle.
- Code publication: append-only code space, sync_other_cores before linking/publishing, jump-table Release
  store, entries can't tear, no host literal pools (t12 confirmed).
- ARM conditional non-branch instructions emitted as branch-around: native predication is also only a
  control dependency, so no ordering promise is lost (t12).

## Known real gaps (found by t12; being fixed one by one, none shown to cause this)
1. Shift shortcuts drop dependencies (thumb.rs ~279, ~350-352, ~361).
2. Unicorn fallback splits aligned LDRD/STRD (vendor translate.c ~9174/~9214).
3. Value-based exclusives (ABA) and the record survives safe-point parking (runtime.rs ~738).
4. mprotect to writable doesn't drop the FIXED mark (libc_calls.rs ~1681).
Not repairable by a barrier prefix: 2, 3, 4. Only 1 is closed by ishld (not ishst).

## Leads not yet followed
- Find WHICH guest code needs the extra store ordering: e.g. emit the barrier only in blocks from certain
  regions (libmonobdwgc vs libmono JIT code vs libunity) via a debug switch and bisect by address range;
  with ~13% base rate each split needs ~20-30 runs.
- Mono JIT output was never scanned (only shipped .so files); a dump of JIT code regions could be checked
  for patterns (dependency through shifts/ands, LDRD via fallback, ldrex/strex split across blocks).
- Check whether any guest store is emitted as two host stores or moved past another store (e.g. STM,
  VSTM, unaligned paths, store + post-increment writeback ordering) in code that runs during loading.
- Count fallback (Unicorn) instructions during loading with debug.arm32bridge.stats=1 and look for
  memory instructions among them.
- An `rr`-like record is not available on the phone; a cheaper probe is a guest-side free-list check
  (walk GC_obj_kinds freelists after each GC_lock acquire) to catch the corruption earlier.

## How to reproduce
- Install the build, then: debug.arm32bridge.fence = "" (none) / nop / st / ld / 1 via adb setprop
  (once the default is the light barrier, the switch value "off" turns it off).
- ~/dev/arm32-bridge-games/scripts/cw-crashloop.sh <name> 10 45; logs in ~/dev/arm32-bridge-games/runs/<name>/;
  count deaths with grep -E "\[fault\]|F arm32|Assertion".
- Scripts used: /tmp/a32k/round14.sh, round16.sh, round17.sh (may be cleaned by the daily temp cleanup;
  they are short, see round16 text above).

### Round 17 (2026-10-13): light barrier (fence st) as the candidate fix, round-14 install
- Pass rule set before running: 0 deaths in 20 clean runs (with round 14's 0/10 that is 0/30; chance at a 1-in-8 rate ~1.5%). Any death = the light barrier fails.
- Result: r17-st-1 10/10 alive, r17-st-2 10/10 alive. The owner touched the phone during r17-st-1 run 4 (swipe on Card Wars at 21:31:38) and run 5 (home screen at 21:31:58): both alive but excluded, 2 replacement runs queued (r17-st-3).
- Speed (median of last 20 [fps] lines per run, median over runs): st 70.9 / 69.3 fps, own code 9.15 / 9.41 ms; without a barrier (round 16 off/nop) 72.4 / 74.7 / 71.6 fps, own 8.95 / 8.67 / 9.02 ms. Cost ~4% of own-code time, ~3-4% fps.
- Light barrier made the default (thumb.rs fence_prefix: parallel -> dmb ishst; "off" disables; one log line "[bridge] block start prefix: ..."). r17-st-3 replacement runs: 2/2 alive, so 20 clean runs, 0 dead.
- Review t13 (GLM 5.3 Flash, read-only): no defects. Every entry (linked exits, pending sites, jump table, dispatcher) lands on the block base = the prefix; link-site offsets and the fault `starts` table stay consistent; parallel() and log::line are safe inside the OnceLock; checker mode and the PC build get no prefix. Caveat it raised: ishst orders store->store only; if the crash ever comes back with it on, retest with "1" (dmb ish). A prefix only in blocks with stores would not be equivalent.
- Round 18 (default light barrier build, full checks): PC self-test default + forced fallback pass (old PC build fails the new signal-frame test, as expected); phone self-test 0 failures in all six modes, ARM32_CHECK=all 2,023,747 blocks / 0 differences; Card Wars fresh install, no property set: 10/10 alive, every log shows "block start prefix: dmb ishst"; Puzzle Craft 1 119.4 fps (min 116.9), Puzzle Craft 2 119.8 (min 117.6), save files unchanged. The crash fix is DONE (workaround; root cause still open, see the OPEN section).

### Round 19 (2026-10-13): gap 1, shift shortcuts keep the data dependency
- Change (thumb.rs): LSR #32 by immediate is now `mov w,rm; lsr x,#32` (was the constant 0); register LSL/LSR by 32 or more give `eor w,T5,T5` (result and carry), not a constant; register ASR clamps the amount with flag-free arithmetic (`T2 |= (0 - (T2 >> 6)) & 63`) instead of cbz + mov. The first ASR version used cmp + csel, which would clobber the guest flags kept in host NZCV; replaced before building.
- Expected: no change in behaviour (results are the same values), no measurable speed change (paths are rare). This closes gap 1 (native ARM keeps an address dependency through such results).
- New self-test check "shifts by 32 or more (register amounts up to 255, LSR/ASR #32), ARM and Thumb": LSL/LSR/ASR/ROR by register, amounts {0,1,31,32,33,63,64,65,127,200,255,259}, values {0x80000001, 0x7FFFFFFF, 0xDEADBEEF, 1}, plus LSR/ASR #32.
- Results: PC self-test default + forced fallback 0 failures (new check ok); phone 0 failures in all six modes, new check ok in all six, ARM32_CHECK=all 1,715,026 blocks / 0 differences; Card Wars r19-shift 3/3 alive (prefix dmb ishst); Puzzle Craft 1 119.4 fps (min 115.6), Puzzle Craft 2 119.8 (min 118.0), save files unchanged.
- Conclusion: passes. Review t14 (Sol 6.1 high) pending.
- Review t14 (Sol 6.1 high, read-only): no translator bug. Its own model of all amounts 0-255 x 4 kinds x 69 values x both carries (141,312 cases) matched; no flag or scratch clobber; `eor w,x,x` keeps the dependency (Arm's own litmus example). Gap: the self-test only used plain (non-S) forms and checked results, not flags.
- Fix (test only): the shift self-test now runs every form with chosen flags going in and reads the flags after: plain and S forms, LSL/LSR/ASR/ROR by register, amounts 0-259, 6 values, 4 flag sets, plus LSR/ASR #32 (plain and S) in ARM and Thumb. Expected: S forms set N, Z, C (C kept when the amount's low byte is 0) and keep V; plain forms keep all flags.
- Round 19b (test-only, /tmp/a32k/round19b.sh): PC default + forced fallback ok; phone six modes 0 failures, the wider shift check ok in all six; ARM32_CHECK=all 2,752,614 blocks / 0 differences. Gap 1 DONE.

### Gap 2 (2026-10-13): Unicorn LDRD/STRD at an 8-byte aligned address as one 64-bit access
- Where: vendor/unicorn-engine-sys/qemu/target/arm/translate.c, new do_ldrd_load / do_strd_store used by trans_LDRD_rr, trans_STRD_rr, op_ldrd_ri, op_strd_ri (copy of the old file: /tmp/a32k/translate.c.before-gap2).
- What: address & 7 == 0 -> one tcg_gen_qemu_{ld,st}_i64 (MO_Q), split/joined with extr/concat (Rt = lower address word; BE swaps halves); otherwise the old two 32-bit accesses (ARM only promises two single-copy-atomic words there, and one host 8-byte access at a 4-aligned address that crosses 16 bytes is only byte-atomic on the host). The branch ends the TCG basic block, so the address and both loaded words are local temps; writeback offset now 0 (the address is no longer bumped by 4). Like upstream QEMU 8 ("Use tcg_gen_qemu_{ld,st}_i64 for LDRD/STRD", MO_ATOM_SUBALIGN), which this QEMU 5 copy lacks.
- How much it matters (checked before testing): small. The translator already does every valid LDRD/STRD as one LDP/STP. Only UNPREDICTABLE forms (Thumb rt == rt2, ARM rt = LR, load with writeback into a loaded register, ARM register offset equal to a loaded register) fall back to Unicorn in translator mode; ARM32_ENGINE=unicorn runs guest threads one at a time under the guest lock, so a tearing test cannot fail there. So no tearing test was added: none could tell the old code from the new.
- New self-test check (correctness of both paths, known answers): "LDRD/STRD at 8- and 4-byte aligned addresses: offsets, writeback, base as first register, ARM and Thumb" (+ ARM register offset). It runs through the patched Unicorn code on the PC and in ARM32_ENGINE=unicorn; ARM32_CHECK=all runs every block in both engines and compares.
- Expected: no behaviour change in any mode, 0 differences in the checker, games unchanged.
- Review t15 (GLM 5.3, read-only): no bugs. Temp lifetimes right (everything read across the branch is a local temp, every temp freed once), writeback offset 0 right, rt == rn safe (base copied first, registers written only at the end), BE order matches gen_load_exclusive, aligned 8-byte access never crosses a page, hooks size-agnostic, all self-test expected values traced by hand. Side gain: a fault on the second word no longer leaves Rt half-written.
- The owner asked to do the last small gaps together with one test round (2026-10-13). Round 20 (gap 2 alone) was stopped before its phone part; round 21 tests gaps 2+3+4.

### Gap 3 (2026-10-13): forget the LDREX record when parked at a safe point
- runtime.rs safe_point: after a thread parks for a stopped world, `clear_exclusive(uc)` (the kernel's return to user space does CLREX after any interrupt). A STREX after the pause then fails and its loop retries, so a value changed and changed back while parked is not missed. Already done before: phone calls, system calls, signal handlers, thread start.
- No self-test: whether a parked thread's STREX fails needs a stop-the-world in the narrow window between LDREX and STREX; not reproducible on demand. Covered by the existing atomics/counter tests for "nothing breaks".

### Gap 4 (2026-10-13): mprotect to writable un-fixes read-only library memory
- mem.rs `unfix(start, end)` removes the range from FIXED (splitting segments). libc_calls.rs mprotect: PROT_WRITE on any fixed bytes -> unfix, log "[bridge] game made read-only library memory writable ...", runtime::note_code_change (Unicorn), dynarec::invalidate_library (engine.rs: drops every block whose span is within LITERAL_REACH bytes of the range (first 4096, raised to 8192 after review t16), since nearby blocks may have folded a literal from it). Retranslated blocks there are in made_blocks (not fixed), so a later cache flush finds them and their literals are real loads. invalidate and invalidate_library share drop_blocks.
- New self-test "library code made writable (mprotect), patched and flushed: new instruction and new literal seen": an ARM function in the self-test library's own .text (`ldr r0, =lit; add r0, #0; bx lr`), called once, then mprotect RWX, patch the add to #1 and the literal to 0x22222222, flush, expect 0x22222223, patch back, expect 0x12345678. On the translator without the fix the old block (not watched, literal folded) would keep returning 0x12345678.
- Round 21 (gaps 2+3+4, first version): PC default + forced fallback exit 0; phone translator, FORCE_FALLBACK_SYNC, CHECK=all 0 failures; stopped there to apply the t16 fixes below (the later game steps rebuild from the tree).
- Review t16 (Sol 6.1 high, read-only): gap 3 fine (clears the live state for both engines; STOP only comes from drop_blocks, not every translation, so no livelock). Gap 4 had three real edge cases, all fixed at once:
  1. A block starting in a fixed page and running into the unfixed page was not watched after retranslation (made_blocks used only the first address). Now `mem::fixed_range(start, end)`: watched unless the whole block is in one fixed segment.
  2. 4096 was too short: ARM `ldr pc, [pc, #4092]` reads 4100 bytes past its instruction (PC + 8). LITERAL_REACH is now 8192.
  3. Two overlapping mprotect calls: the second saw nothing left to unfix and returned before the first dropped the old blocks. Now the unfix + drop runs under one lock (UNFIXING), so the second waits. The waiting thread is in an import (marked outside guest code), so stop_world doesn't wait for it.
- Round 22 = round 21 with these fixes (job 005).
- Round 22 results (gaps 2+3+4 with the t16 fixes, /tmp/a32k/round22.sh): PC default + forced fallback 0 failures; phone 0 failures in all six modes; ARM32_CHECK=all 2,770,148 blocks / 0 differences; the new checks "LDRD/STRD at 8- and 4-byte aligned addresses ..." and "library code made writable (mprotect) ..." ok on PC default, PC forced fallback and all six phone modes (8/8 each); Card Wars r22-gaps 3/3 alive (prefix dmb ishst); Puzzle Craft 1 119.6 fps (min 117.4), Puzzle Craft 2 120.0 (min 117.4), save files unchanged. Gaps 2, 3, 4 DONE. All four t12 gaps are closed (the light barrier stays; the root cause is still open, see the OPEN section).

### Final review t17 (Claude Fable 5.1, high, read-only, 2026-10-13)
- No correctness bug in the light barrier or gaps 1-4. Gap 2 temps, gap 3 live-state clearing, gap 4 locking (UNFIXING before STOPPER, waiter is outside guest code, no deadlock), reach 8192 >= worst ARM case 4106 bytes, only one literal-folding site (thumb.rs load_store), raw `svc` mprotect is ENOSYS so no bypass.
- Statistics corrected: ishst record is 0/48 launches (r14 10, r17 22, r18 10, r19 3, r22 3) vs 9/70 without: Fisher exact ~0.7%, so the default is supported, but the 95% upper bound on the remaining crash rate is ~6%: "not seen in 48 launches", not "never". All barrier runs so far 0/78 vs 9/70 (~0.1%).
- Open question it raised: ishst orders store->store only, yet ishld was also 0/10; no single missing ordering is fixed by both. Either the ld result was chance (p ~ 0.25 at 13%) or the mechanism differs. Cheapest next experiment: 30 runs with fence=ld. Also: the prefix sits only at block starts, so two stores inside one block get no barrier between them; the barrier only separates stores in different blocks (fits a "build it in a call, publish it after return" pattern).
- Fixed at once: LSR #32 relied on the host register's upper half being 0 (`mov w` skipped when result == rm, then `lsr x, #32`). True today (guest registers only get W writes) but not enforced; now two W shifts (`lsr w,rm,#31; lsr w,w,#1`), still computed from rm.
- Fixed: thumb.rs fence_prefix comment ("not once in 48 launches"), the p-value and the stale LITERAL_REACH text above.
- Done as it asked: show the mprotect self-test fails without gap 4 (round 23 runs it on a build of the last commit, 06c499c).
- Round 23 (b), current tree with the LSR #32 two-W-shift change: PC default + forced fallback 0 failures (shift check ok); phone 0 failures in all six modes, shift check ok in all six; ARM32_CHECK=all 2,767,131 blocks / 0 differences. (Games not rerun for this one-instruction change; the shift self-test covers LSR #32 in ARM and Thumb, plain and S.)
- Round 23 (a), the control (06c499c + new self-test): first try built the wrong tree (log shows the ~/dev path, cause unclear; cargo in the copy resolves correctly), retried as round23b.
- Round 23b (control, 06c499c built in /tmp/a32k/head-src, new self-test, translator mode): "FAIL library code made writable (mprotect) ..." as predicted, so the test catches the missing gap 4. Also FAIL (expected, fixed earlier and not in that commit): signal frame on the thread's own stack, patched literal in made code. Shift and LDRD/STRD checks pass there (their values never changed; they guard correctness, not the dependency).
- State at the end of this round: light barrier default (workaround, root cause open), gaps 1-4 closed, all checks green, phone crash-box setting restored. Card Wars speed with the barrier ~70 fps (target 115 not reached).

# Debugging: Card Wars stutters during real play (2026-10-14)

## Symptom
Owner: fps is fine now, but actual play has lots of stutters ("it will be obvious"). Idle runs never showed it.
Build a2abb57 (installed lib md5 = local build). Play sessions recorded with the owner playing:
`~/dev/arm32-bridge-games/scripts/cw-play-capture.sh <tag> 180` (logcat -v epoch + simpleperf 1000 Hz all threads;
`--trace-offcpu` is not available on this phone: no sched:sched_switch). Runs: runs/play1, play3 (play2 = sampler crashes).

## Measurements
- play1 (281 s after loading): 75 seconds with a frame > 50 ms; worst 250-400 ms. Grouped by "stops of every
  thread" in that second: 0 stops -> median slowest 27 ms (9/121 s > 50 ms); 201+ stops -> median slowest 155 ms
  (28/32 s > 50 ms). So freezes come with bursts of rewritten game-made code (Mono patching JIT code -> cacheflush).
- But the stops are not what costs the time: UnityMain is on-CPU ~85% in both calm and freeze seconds, with the
  same mix (translated ~45%, engine::run ~18-19%, park_registers ~9%, call_stub ~8%, kernel futex wake ~8-9%).
  In freeze seconds: translation, membarrier (smp_call_function_many_cond 1.1%) and __clear_cache are ~1-2% only.
  => freezes are bursts of the game's own work, made 2-3x longer by the bridge's general overhead.
- Sampler (debug.arm32bridge.sample, logs stack every 500 ms at phone calls; symbolized with
  scripts/sample-sym.py and the APK's 32-bit libs in ~/dev/arm32-bridge-games/cwlibs): freeze samples show Mono JIT
  (mono_local_regalloc, mono_arch_emit_prolog, mono_spill_global_vars), GC_finish_collection, libz inflate
  (asset unpacking), libunity work. Too coarse (2 samples/s, stale stack words) to rank them.
- Sampler mode 2 (mono_pmip names) is NOT safe: from launch it crashes Mono startup at block 0x026cdb4c
  (libmonobdwgc +0x4aeb4c, fault 0x70), 3/3; switched on after loading it froze the game within seconds
  (UnityMain looping on sem_getvalue from libmonobdwgc +0x502374, likely a GC suspend wait). Mode 1 is fine.
- Unity's public symbol server had no libunity symbols for build id 744f4cc8c578011d941fab6c30eec73d17a21a6e
  (tried SSQP and Breakpad-style paths).
- Phone-call counts (stats1.log, earlier): pthread_getspecific 4.1M, strcmp 2.1M, memset 1.4M, mutex unlock 1.3M,
  lock 1.0M, memcpy 0.9M, free 0.7M, calloc 0.5M, __memmove_chk 0.4M, clock_gettime 0.4M, malloc 0.3M ...

## Plan (owner chose A, 2026-10-14)
A: light path for tiny phone functions (no register parking, no leave/enter guest, no slow-call timing).
B (later, maybe): look up indirect jump targets inside translated code instead of the dispatcher.
Pass checks: 3 baseline runs first; loading burst (launch->READY, load work, slow seconds) and steady game
ms/frame must improve beyond noise; self-test 0 failures in six modes; three games run; PC saves unchanged.

## Fix A results (light phone functions, 2026-10-14)
- Change: runtime.rs `LIGHT` list + `call_light` (no parking / leave-enter / timing for memcpy-like, string,
  heap, TLS key, mutex trylock/unlock, clock_gettime; pthread_mutex_lock in place only when free); engine.rs tries
  `call_light` before `call_stub`; threads.rs `lock_if_free`. Only in parallel mode, never on a stub's first call,
  never while tracing/sampling.
- Checks: PC self-test default + forced fallback 0 failures; phone six modes 0 failures; CHECK=all 2,835,899 blocks,
  0 differences.
- Loading (cw-measure.sh 90 s, load-summary.py), 3 runs each:
  before: READY 19.9/19.0/19.0 s, load work 28.5/28.3/28.2 s, slow (>100 ms) seconds 13/12/12, steady 14.06/14.00/14.32 ms
  fix A:  READY 16.9/18.6/18.0 s, load work 27.0/28.7/28.2 s, slow seconds 11/12/12, steady 12.78/12.90/12.87 ms
  => steady game time -9% (beyond noise); load work and slow seconds unchanged (fails that half of the pass check).
  During loading, phone time fell 9.6 -> 7.8 s but own-code time stayed ~19-20 s; 2-3 s loading frames are 1.4-2.0 s own code.
- Scripted play (scripts/cw-scripted-play.sh, same taps; both ended on the Card Forge screen), 225 s:
  old 67.7 fps, >50 ms 60 s, >100 ms 6 s, >250 ms 1, worst 312 ms; fix A 74.5 fps, 56 / 8 / 1, worst 317 ms.
  => more fps, same freezes.
- My own play on the old build (runs/myplay-old): big freezes are first-time screens (menu 356 ms, quest map 370,
  level popup 298, battle setup 232, lose screen 763); repeated battle actions stay smooth.
- Loading profile with fix A (runs/loadprofA, 35 s from launch): UnityMain translated 50%, engine::run 17.8%,
  kernel futex/IPI ~7%, smp_call_function_many_cond 3.2% (+__clear_cache 1%), host malloc ~3%.
  Inside engine::run ~65% is the SLOW lookup (RwLock read lock/unlock atomics + HashMap probe): jump-table misses.
  Cause: JUMP_TABLE_BITS = 12 (4096 entries indexed by pc bits 1-12), so targets 8 KiB apart collide; 18 MB of
  libunity + Mono JIT code thrash it. Candidate next change: bigger table (one constant; emitter uses it).
- Owner kept fix A (2026-10-14). Puzzle Craft with fix A: PC1 119.9 fps (min 119.5), PC2 119.7 (min 115.9), no
  crashes. "Saves changed" was only SDK files rewritten on every launch (Crashlytics/Flurry/Fabric .tap/.cls,
  AppEventsLogger, Chillingo-SharedData, FusePrefsFile); real save files unchanged. pc-check.sh now skips those.

## Bigger jump table (owner approved, 2026-10-14)
- Change: engine.rs JUMP_TABLE_BITS 12 -> 16 (65,536 entries, 512 KiB at the start of the 64 MiB code memory).
  The emitter (thumb.rs emit_exit_to ubfx width), table init, set_jump/table_block all use the constant.
- Checks: PC self-test default + forced fallback 0 failures; phone six modes 0 failures; CHECK=all 2,810,938 blocks,
  0 differences.
- Loading x3 (vs fix A alone): READY 15.4/15.6/15.5 s (16.9-18.6), load work 26.9/26.8/26.9 s (27.0-28.7),
  slow seconds 10/11/11 (11-12), steady game 9.99/10.17/10.13 ms = 98.5-100.4 fps (12.8-12.9 ms, 78 fps).
- Scripted play (same path, same Card Forge end screen), 224 s: 88.3 fps, game 11.54 ms, >50 ms 25 s, >100 ms 3 s,
  >250 ms 1, worst 257 ms. (old 67.7 fps 60/6/1 worst 312; fix A 74.5 fps 56/8/1 worst 317.)
- Puzzle Craft: PC1 119.9 fps (min 119.0), PC2 119.9 (min 118.7), no crashes, real saves unchanged.
- Remaining: loading frames of 0.5-2.5 s still happen (own code); first-time screens still hitch, smaller.
  Next leads: per-new-block membarrier (smp_call_function_many_cond ~3% + __clear_cache 1% while loading), the
  rest of engine::run, translated code quality (~50% of the main thread).

## Owner: "still very noticeably laggy" (2026-10-14, after jump table)
- Owner session (runs/ownerplay-jt16, 216 s after loading): median 87 fps, p10 68; 178 s had a frame >17 ms,
  58 >25 ms, 31 >33 ms, 14 >50 ms, 5 >100 ms, 3 >250 ms.
- Frame trace (Perfetto android.surfaceflinger.frametimeline, scripts/cw-trace-play.sh, runs/trace-jt16,
  60 s inside the scripted battle tap loop). Gaps between frames reaching the screen (5363 frames):
  <=9 ms 3841, ~17 ms 1364, ~25 ms 99, ~33 ms 26, ~50 ms 24, >50 ms 8.
  => motion is uneven all the time: 8.3/16.7 ms mixed (example run: 8.3 8.3 8.4 16.7 8.3 8.3 8.4 8.3 33.4 ...),
  ~23 doubled frames per second, plus ~2.6 gaps per second of 25 ms or longer. Long gaps are not on a 1 s clock.
- Per-second [fps] lines in the battle loop: slowest frame p50 33.6 ms (own code 30.4, phone 3.3), p75 45 ms,
  p90 52 ms. So in most seconds one frame takes 3-6x the median 10 ms, almost all of it translated code + engine.
- Kernel has no trace events (/sys/kernel/tracing/events missing): no scheduler data from Perfetto or simpleperf.
- Old play profile (play3) per-core: UnityMain ~88% on the big core (cpu7), ~12% on mid cores; render thread
  (arm32-guest-108, GLES calls) on mid/little cores 0-6. Guest threads 102/103 burn ~1.1 cores in kernel
  wakeups + a small guest loop (+0x...896c4ec0/4f38) + wait_until: looks like spin/wait churn.
- Display offers 60 and 120 Hz only.
- Correction: the "battle tap loop" in cw-scripted-play.sh does not reach a battle any more; it taps through menus
  and ends on Card Forge (owner confirmed: menu actions, no battle). trace-jt16/trace2-jt16 are menu play.
- trace2-jt16: Perfetto frames + simpleperf 2000 Hz together (60 s). Clocks lined up with /proc/uptime (boottime)
  vs /proc/timer_list "now" (monotonic): offset 10930.24 s. Scripts: analysis/slow-frames.py, core-vs-gap.py.
  * Slow gaps vs normal: UnityMain runs ~1650 samples/s in both (it is busy nearly all the time; CPU-bound).
    What it runs is the same mix (translated ~64%, kernel wake ~12.5%, engine::run 6%, clock_gettime vdso 4%).
    Difference is WHERE: in slow windows UnityMain is on the big core (cpu7) 52% of samples, normal 74%.
  * Per frame gap, by UnityMain's big-core share in that frame (lag 8 ms; 0 and 16 ms give the same picture):
      0-20%: 711 gaps, 10.8% slow (>24 ms), mean 15.1 ms | 20-40%: 190, 13.7%, 14.9 ms | 40-60%: 196, 8.7%, 13.3 ms
      60-80%: 406, 3.7%, 11.2 ms | 80-100%: 3648, 2.1%, 10.7 ms
    => about one frame in five runs mostly on a mid core (A725 3.05 GHz vs X4 3.78 GHz); those are 40% slower and
    5x more likely to hitch. Roughly half the long gaps come from these frames.
  * cpu7 clock while the game sat on Card Forge: 3.40, 2.94, 0.88 GHz in three 1 s reads (governor ramps down).
    Thermal status 0 (no throttling).
  * Mono GC (Boehm stop-the-world, signals 24/30 to ~5 threads): 261 collections in 193 s; seconds with 0 GCs still
    have median slowest frame 33 ms, seconds with 4+ GCs 55-178 ms. => GC causes the biggest spikes only.
  * Thread groups "arm32-guest-102"/"103" (comm is truncated: handles 0x102x/0x103x, 4 threads each, Unity worker
    threads t9-t15) use ~1.5 cores together on a still menu: futex syscalls + kernel wake-ups + guest code.
- No ADPF performance hint, uclamp or affinity use in the bridge (grep).
- Sub-agent review (2026-10-14, codex gpt-6.1-sol, read-only): 
  * My clock offset was 45 ms off: trace clock snapshots give BOOTTIME-MONOTONIC = 10930.285099031 s (use this).
    Corrected, the association stays: UnityMain cpu7 share 0-20%: 688 gaps, 13.8% >24 ms, mean 16.14 ms;
    80-100%: 3702 gaps, 2.0%, 10.63 ms. But a window 48 ms after the present shows it too, so it's a link
    between busy stretches and placement, not proof that moving cores causes the slow frames.
  * layer_name IS NULL rows are SurfaceFlinger display frames (all layers), not the game's own frames.
  * Gap counts recomputed: trace-jt16 89.4 updates/s, 2.58 gaps >24 ms per s; trace2-jt16 86.3, 3.50 per s.
  * Worker TIDs under names 102/103: 86.9 CPU-s in 60 s (~1.45 cores). UnityChoreographer 4.6 CPU-s.
  * Ranking for what a player feels: B (real 60 fps pacing, not just setFrameRate: Android says setFrameRate
    does not throttle) > A (performance hint) > C (honour affinity: cpu2-7 mask barely changes UnityMain,
    which is on cpu0-1 only 0.47% of samples).
  * Suggested: causality test with UnityMain held on cpu7 vs not (diagnostic only); real battle captures.
- Core causality test (diagnostic only, no code change): scripts/cw-trace-play.sh with PIN=80 runs
  `taskset -p 80 <UnityMain tid>` (cpu7 only) at READY+75 s, through the end; Perfetto 60 s frames, no sampler.
  Alternated pin, normal, pin, normal (analysis/gaps.py):
    pin7-1: 111.1 updates/s | 17 ms gaps 6.7/s | >24 ms 0.80/s | >33 ms 0.12/s | >50 ms 0.02/s | max 58 ms
    ctl-1 :  92.8           | 20.0/s            | 2.55/s        | 0.40/s        | 0.13/s        | max 175 ms
    pin7-2: 107.7           |  8.7/s            | 1.12/s        | 0.28/s        | 0.03/s        | max 150 ms
    ctl-2 :  89.1           | 22.6/s            | 2.95/s        | 0.52/s        | 0.08/s        | max 175 ms
    (earlier normal: trace-jt16 89.4 / 22.8 / 2.58; trace2-jt16 86.3 / 24.5 / 3.50)
  Whole-session [fps] (pin covers the last ~150 of 225 s): pin 100.8 / 97.6 fps, normal 88.2 / 86.9 fps.
  => CONFIRMED: keeping the main thread on the fastest core cuts doubled frames ~65% and long gaps ~65%.
  Core placement is a cause, not just a side effect. Pinning itself is only a diagnostic (device-specific).

## Fix: Android performance hint for the frame thread (owner chose A, 2026-10-14)
- Change: new src/perf_hint.rs (Android only). On the first nativeRender frame it opens an APerformanceHint session
  (looked up with dlopen/dlsym in libandroid.so) for the thread running the frame, target = 1/TARGET_REFRESH_RATE
  (8.33 ms), and after each frame reports the frame's time minus time spent in eglSwapBuffers on that thread
  (egl.rs times each swap). If frames move to another thread, the session follows. debug.arm32bridge.hint=0 = off.
- Known limit: Unity swaps on its render thread, so UnityMain's report includes waits for that thread.
- Pass check: 2 hint + 2 normal (hint=0) runs alternating with cw-trace-play.sh (menu path) + gaps.py; doubled
  frames and >24 ms gaps clearly below every normal run so far (17 ms 20.0-24.5/s, >24 ms 2.55-3.50/s); self-test
  0 failures (PC + six phone modes); Card Wars loading x3 not worse; Puzzle Craft 1/2 fps and real saves unchanged.
- Result (owner kept it after one run, 2026-10-14; owner rule: two runs pointing to a gain is enough for speed
  changes): hint-1 105.2 updates/s | 17 ms gaps 10.7/s | >24 ms 1.28/s | >33 ms 0.32/s | >50 ms 0.05/s | max 200 ms
  (normal runs 86-93 / 20-24.5 / 2.55-3.50). Remaining comparison runs stopped at the owner's request.
- Checks: PC self-test 0 failures; phone six modes 0 failures (CHECK=all 2,854,559 blocks, 0 differences);
  Puzzle Craft 1 120.0 fps (min 119.8), PC2 119.9 (min 119.1), no crashes; PC1 saves unchanged.
  PC2 "CHANGED" = SDK files (Flurry, AppEventsLogger, Crashlytics .cls, .tap, FusePrefsFile, WebViewChromiumPrefs)
  plus files/stgs.atgsave, which PC2 rewrites as 0 bytes on every launch (not touched on Home/stop). Main save
  pc2.atgsave unchanged (6516 bytes, last written 2026-10-13 23:36). OPEN: is a 0-byte stgs.atgsave normal for
  PC2 or a bridge file-write bug? Not related to the hint (frame timing only).

## Cleanup (garbage collection) pauses (owner chose "find why cleanup pauses so long", 2026-10-14)
- Boehm's own stats via debug.arm32bridge.env="GC_PRINT_STATS=1;GC_LOG_FILE=/data/data/<pkg>/cache/gc.log"
  (no code change), scripted menu run runs/gcstats1 (hint on: 105.9 fps, >50 ms seconds 21, >100 ms 4, worst 204 ms).
  Unity uses incremental GC: time-sliced stopped marking ("Abandoning stopped marking after ~3.6 ms"), dirty pages.
  530 collections (48 full); final stopped mark mean 3.3 ms, max 25 ms, total 1755 ms; abandoned slices 227 x ~3 ms
  = 677 ms. Total GC stop time ~2.4 s in ~4.5 min (~1%). Median 2.4 MB allocated between collections.
  => RULED OUT as the cause of 50-200 ms pauses (plumber rule: 3 ms pauses can't make 100+ ms frames). The earlier
  "90% of slow seconds have a GC" was just GC happening in ~65% of all seconds.
- Long screen gaps (>45 ms) in trace2-jt16 with the exact clock offset (analysis/long-gaps.py): 13 gaps
  (50-175 ms). UnityMain inside them: translated 67%, engine::run 7%, kernel wake 4.6%, Args::from_registers 2.5%,
  check_in 1.5%, call_light 1.4% = the same mix as normal frames, just more of it. So long frames are more GAME code.
- Unicorn (fallback engine) shows up on worker threads (tcg_gen_code/tcg_optimize, reg_read/write): 1-2% of samples
  overall (UnityMain 0.8%). Small; not the cause.
- Next need: names for translated code (which game function runs in long frames). Profiles only see "unknown".
- Naming tool (owner approved): debug.arm32bridge.blockmap=<file> / ARM32_BLOCKMAP writes "time host-addr bytes
  guest-pc" per translated block (engine.rs note_block); analysis/named-gaps.py names samples (libunity by 4 KiB
  page, libmono by symbol, rest "mono-jit" = C# code compiled by Mono) and groups them by kind.
- Named profile runs/named1 (hint on, menu path; 99.0 fps, >50 ms seconds 27, >100 ms 7, worst 321 ms). UnityMain,
  10 gaps >45 ms (200, 108, 108, 50-58 ms), share inside long gaps vs overall:
    bridge running/phone calls 18.4 vs 12.9 | libunity 18.1 vs 45.7 | Mono runtime other 13.8 vs 5.5 |
    kernel other 9.5 vs 13.4 | C# code 8.1 vs 7.7 | Mono stack walking/exceptions 6.1 vs 0.3 (mono_handle_exception
    present) | Mono GC 5.5 vs 2.6 | host malloc 5.3 vs 2.1 | glue 4.5 vs 3.4 | Mono JIT compiler 4.1 vs 0.1 |
    new-code sync IPI 2.2 vs 0.0 | Unicorn fallback 2.2 vs 0.6.
  => long pauses are bursts of the game's C# runtime work (exceptions being thrown and unwound, first-time JIT,
  GC marking, runtime lookups) on menu actions, made slower by the bridge's general overhead (~28% of the pause:
  dispatch/phone calls, malloc, Unicorn, IPIs). No single bridge hotspot; the 200 ms gap has the same mix.
- Overall on UnityMain the kernel wake path (_raw_spin_unlock_irqrestore) is 11.5% and clock_gettime 4.8%.

## Thread wake-ups (owner chose "cheaper thread wake-ups", 2026-10-14)
- Call-graph profile of UnityMain (scripts/cw-callgraph.sh, simpleperf --call-graph fp, 30 s menu loop, hint on;
  analysis/kernel-callers.py, runs/callgraph1): kernel 12.6% of samples; _raw_spin_unlock_irqrestore 10.5%, and
  99.6% of that is futex_wake < __arm64_sys_futex. User side: the guest's own raw futex WAKE syscalls
  (linux_syscall -> futex -> wake_on_count -> Parker condvar notify -> libc syscall). Performance hint 0.5%.
  => the main thread spends ~10% of its time waking sleeping helper threads (Unity job workers). wake_on_count only
  makes a host syscall when a waiter is listed, so the bridge isn't adding wakes; the cost is the kernel waking a
  sleeping thread on another core (IPI). The waker also holds the global WAITERS lock during that syscall.
- Fix (owner approved, 2026-10-14): spin before sleeping (runtime.rs wait_until/Slot). Each wait gets a Slot
  {outcome, sleeping}. The waiting thread first watches its outcome for up to 50 us (debug.arm32bridge.spin=<us>,
  0 = sleep at once), then sets `sleeping` under its own lock and sleeps on its condvar. A waker claims the outcome
  under WAITERS as before, lets WAITERS go, and only locks + notifies the thread when `sleeping` is set (SeqCst on
  both sides, so a wake is never lost). A wake inside the spin window is a memory write with no system call.
  Predicted: the futex_wake share of UnityMain drops well below 10.5%; if it stays ~10% the helpers wait longer
  than 50 us between jobs and the spin doesn't help (then try a longer spin once, else revert).
- spin50-cg (call graph, spin 50 us, hint on, menu path; runs/spin50-cg): UnityMain kernel 12.6% -> 5.7%,
  _raw_spin_unlock_irqrestore (futex_wake) 10.5% -> 4.5%. Prediction confirmed: most wakes now land in the spin
  window. (Crush restarted during the run's end; the call graph was already pulled.)
- spin50-t1 (frame trace, spin 50 us + hint, menu path; analysis/gaps.py): 117.0 updates/s | doubled (17 ms)
  1.1/s | >24 ms 0.52/s | >33 ms 0.08/s | >50 ms 0.05/s | max 175 ms. Compare hint-1 105.2 | 10.7 | 1.28 | 0.32 |
  0.05 | 200 and no-hint ctl-1/2 89-93 | 20-23 | 2.6-3.0. Bridge log: fps avg 118.4 (was ~103), game 8.42 ms/frame
  (was 9.74); seconds with a frame >50 ms 22, >100 ms 6, >250 ms 1 (worst 269 ms) = about the same as before.
  => KEPT (call-graph + frame run both show a clear gain). Steady frames are now near a locked 120; the remaining
  stutter is the rarer long pauses (game C# runtime bursts, see "Cleanup pauses").
- Safety checks (spin 50 us): PC self-test 0 failures; phone self-test all six modes 0 failures (checker 3438442
  blocks, 0 differences); pc-check spin50: PC1 120.0 fps, saves unchanged; PC2 119.9 fps, pc2.atgsave unchanged
  (only the 0-byte stgs.atgsave rewritten at launch, same open question as before); no crash lines.

## Bridge overhead inside pauses (owner chose "cut the bridge's extra work in pauses", 2026-10-14)
- runs/named2 (spin + hint, menu path, BLOCKMAP=1 CG=1: cw-trace-play.sh now takes CG=1 for --call-graph fp;
  analysis/gap-callers.py groups host samples by call chain). Only 2 gaps >45 ms in the 60 s window, so used
  >25 ms (25 gaps, 1526 samples). Bridge/host share inside gaps 30.9% vs 29.4% overall: the pauses are more game
  work paying the usual bridge costs, not a pause-only bridge problem.
- Biggest piece: engine::run self time 8.2% of UnityMain inside gaps (6.4% overall). Hot lines (llvm-addr2line on
  the phone .so): ~55% hashbrown probe (vceq/likely) + ~12% RwLock read/unlock + block_for closure (engine.rs:568)
  = the dispatcher's slow path: jump-table miss -> read_cache() RwLock (a counter every thread bounces) -> HashMap
  get -> set_jump. table_block hit check itself only ~4%.
- Why it misses: 253713 distinct block pcs (blockmap), direct-mapped table of 2^16 slots indexed by pc bits 1..16.
  Share of blocks whose slot is shared with another block: 16 bits 95% | 17 88% | 18 71% | 19 47% | 20 28%.
  So hot targets 128 KiB apart keep evicting each other.
- Code memory: 64 MB with the table at the start; translated code 25.6 MB at 30 s, 27.0 MB from 120 s on (menu path).
- Other pieces (inside gaps): guest futex syscalls 2.7%, guest clock_gettime via light call 2.2% (game's own
  time reads), spin wait clock reads 1.3% (waiting time anyway), call_light/Args/write_result ~2.7%, check_in 1.7%,
  malloc ~1%, __clear_cache 0.4%, block-map writes 0.7% (test switch only).
- Fix (owner: "even 32 MB, I have 12 GB"): JUMP_TABLE_BITS 16 -> 22 (table 0.5 -> 32 MB), CODE_SIZE 128 MB (owner: "as
  large as you can without exceeding 128 MB") = 96 MB for code
  (block-to-block B reaches +-128 MB). Predicted: engine::run self time on UnityMain drops from ~6.4% toward
  ~2%, RwLock/hash lines mostly gone; frame numbers same or better.
- jt22-1 (32 MB table, 128 MB code space, spin + hint, menu path, CG=1): engine::run samples on UnityMain 6318 -> 2153
  (same 60 s window). Hot lines now table_block's entry load (engine.rs:291, 22% of the rest = cache/TLB miss on
  the big table); hash probe ~4%. engine::run self share inside >25 ms gaps 8.2% -> 2.4% (overall 6.4 -> 2.1);
  bridge/host share inside gaps 30.9% -> 24.7% (overall 29.4 -> 24.6). Frames (gaps.py): 116.4 updates/s | 17 ms
  1.8/s | >24 0.62/s | >33 0.12/s | >50 0.03/s | max 92 ms (spin50-t1 117.0 | 1.1 | 0.52 | 0.08 | 0.05 | 175;
  named2 117.3 | 1.3 | 0.43 | 0.07 | 0.02 | 83) = same range; steady frames were already near 120.
- jt22-t2 (clean, NOPERF): gaps.py 116.2 updates/s | 17 ms 1.5/s | >24 0.57/s | >33 0.17/s | >50 0.08/s | max 184 ms.
  play-summary (fps | game ms | seconds with frame >50/>100/>250 | worst):
    before: spin50-t1 118.4 | 8.42 | 22/6/1 | 269 ms; named2 (profiled) 118.1 | 8.45 | 27/5/1 | 322 ms
    32 MB:  jt22-1 (profiled) 117.7 | 8.48 | 33/4/0 | 214 ms; jt22-t2 118.2 | 8.45 | 29/4/0 | 207 ms
  => MIXED: worst pauses shorter (no >250, fewer >100, worst ~210 vs 270-320) but more 50-100 ms seconds in both
  runs; game ms/frame unchanged despite ~4% less dispatcher time. Possible cost: the 32 MB table's entries miss the
  cache/TLB on every lookup (table_block's load is now the top line). Not kept yet: owner decides.
- Safety (32 MB): PC self-test 0 failures; phone six modes 0 failures (checker 3727354 blocks, 0 differences);
  pc-check jt22: PC1 119.8 fps saves unchanged, PC2 119.7 fps (stgs.atgsave only), no crash lines.
- Owner: "Try 8 and 16". Clean runs (NOPERF), CODE_SIZE 128 MB (play-summary fps | game ms | >50/>100/>250 | worst;
  gaps.py >33/>50/max):
    8 MB (20 bits):  jt20-t1 118.2 | 8.43 | 29/4/0 | 217 ms; >33 0.07 >50 0.02 max 58
                     jt20-t2 118.1 | 8.44 | 33/4/0 | 213 ms; >33 0.20 >50 0.07 max 117
    16 MB (21 bits): jt21-t1 118.2 | 8.43 | 30/3/0 | 226 ms; >33 0.10 >50 0.07 max 125
                     jt21-t2 118.1 | 8.44 | 31/3/0 | 210 ms; >33 0.07 >50 0.05 max 175
  => 8/16/32 MB all alike. Same-session control with the old 16-bit table running (jt16-ctl1/2) to tell table
  effect from phone state (the "before" runs were ~1 h earlier).
- Same-session control, old 16-bit (0.5 MB) table, CODE_SIZE 128 MB:
    jt16-ctl1 116.9 | 8.54 | 31/5/0 | 241 ms; 114.2 updates/s, 17 ms 3.3/s, >33 0.13, >50 0.07, max 242
    jt16-ctl2 117.9 | 8.46 | 35/4/0 | 235 ms; 115.7 updates/s, 17 ms 2.0/s, >33 0.20, >50 0.07, max 175
  => The extra 50 ms seconds vs the morning runs were phone state (the old table shows them too). Against the
  same-session control every bigger table is better: updates/s 116.2-117.3 vs 114.2-115.7, doubled frames
  1.3-1.8/s vs 2.0-3.3/s, >100 ms seconds 3-4 vs 4-5, worst 207-226 vs 235-241 ms. KEPT; 8/16/32 MB alike.
  Chose 16 MB (21 bits): 15% slot sharing, 112 MB left for code, fewest >100 ms seconds (3, 3).
- Safety (16 MB, final): PC self-test 0 failures; phone six modes 0 failures (checker 3716843 blocks, 0 differences);
  pc-check jt21: PC1 119.9 fps saves unchanged, PC2 120.0 fps (stgs.atgsave only, emptied on launch as before;
  pc2.atgsave untouched), no crash lines. Card Wars reinstalled with this build.
