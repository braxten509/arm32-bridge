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
