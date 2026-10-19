# Debugging: IB2 crashes when the second fight starts (Pixel only, plan item 1)

## Symptom
Expected: walking into the second fight (bamboo stairs, after the first
save) starts the fight. Actual (Pixel 10, Game Bridge window, 2026-10-18
15:57): the drawing thread (3103) dies with signal 7 right after
`framebuffer 11 made` (attachment texture 200): a virtual call through an
object whose first 8 bytes are junk (`ldr x8,[x19]; ldr x8,[x8,#0x78];
blr x8` at 0x100732bd0, x8=0xbf4f7906bf151f5d). Caller 0x100738fd0, which
takes x19 from [x20+0x10]. About 207 s into play, frame ~12100.
Emulator (x86_64 + ARM translation, host GPU), same build, same save:
the fight starts fine ("TAP TO DODGE"), framebuffer 11 made, no crash.
Owner's save copy: ~/.cache/ios-debug/phone-save (opens right before the
fight; tapping the move circle at 274,174 points walks into it).

Test setup differences from the user: no-window runs (`run-phone.sh`)
draw to an off-screen surface, not the window.

## Suspects (ranked)
1. Phone-only memory difference: GrapheneOS hardened_malloc (strict, wipes
   and quarantines freed memory) turns a hidden use-after-free or overrun
   in our stand-ins or the game into a crash; the emulator's allocator
   hides it.
2. GPU difference (PowerVR on the Pixel vs host GPU): a GL answer we pass
   through differs (framebuffer status, a query, a format), sending the
   game down a path that frees or never makes the object.
3. A thread race that only shows on the phone's real cores (the emulator
   translates ARM code and runs it with different timing).

## Attempts
### 1. Same save, emulator, Game Bridge window, tap into the fight
- Predicted: crash = not phone-specific.
- Actual: fight starts ("TAP TO DODGE"), framebuffer 11 made, no crash.
- Conclusion: phone-only or rare. Not reverted (no change).

### 2. Same save, Pixel, no window (run-phone style), tap at 28 s and 31 s, 75 s runs
- Script: ~/.cache/ios-debug/phone-fight.sh (fresh copy of the save each run).
- Actual: run 1 and run 2 reach the fight, framebuffer 11 made, no crash.
- Conclusion: not every time; may need longer play (owner crashed at 207 s).

### 3. Read the crash site
- 0x100738f9c is a queued drawing-thread job (returns its size 0xb0): it
  takes an object from job+0x10, checks its byte at +0x180, calls
  0x100732bb0 (virtual call at vtable+0x78 on the object), then copies a
  4x4 matrix and bounds into it: an "update this object's transform" job.
  The object's vtable word was junk, so the object was freed/reused
  before the job ran, or the job read a stale/junk pointer.
- The game has no atomic or barrier instructions of its own (0 ldxr/ldaxr/
  stlxr/dmb): it relies on OSAtomic*/OSMemoryBarrier/locks; ours use
  SeqCst Rust atomics and a SeqCst fence (look right).
- `vm_map`/`mach_make_memory_entry_64` (refused here) are never called in
  either log: ruled out.
- Phone tells the game 36 extensions, emulator 27 (chip-dependent). Added
  a log of the chip-provided ones and test switch `IOS_GL_HIDE`.
- Added to the crash report: for every register pointing at readable
  memory, its mapping and the 64 bytes around it.

### 4. Run 3 of the 75 s no-window runs: no crash (3 of 3 clean)
- Difference found: no-window runs (shell process) get heap pointers with
  Android's 0xb4 top-byte tag; the Game Bridge window process gets
  untagged pointers (GrapheneOS: memtag_heap on, app flag
  ALLOW_NATIVE_HEAP_POINTER_TAGGING). The crash was in the untagged one,
  so tags don't explain it; noted as a setup difference.

### 5. Named the job and checked memory-mapping stand-ins
- The job's vtable (0x100b9b810) has DescribeCommand -> "UpdateLightTransform"
  (UTF-32 at 0x100a381c0). So x19 is a light's scene info (FLightSceneInfo)
  whose vtable word was overwritten: the light was freed/reused, or the
  pointer in the job was stale.
- Fixed-address mapping (Linux MAP_FIXED replaces live memory, Apple
  fails instead): the game's 5 vm_allocate calls all pass VM_FLAGS_ANYWHERE
  (w3=1); its 2 mmap calls pass address 0, flags 0x1002. Ruled out.
- Locks: fix_mutex builds the host lock aside under a global lock and
  publishes the first word last (fix_cond only clears Apple's marker
  atomically; bionic's zeroed cond is valid); Apple struct sizes (mutex 64, cond 48,
  attr 64) are >= bionic's (40, 48, 56). Look right.

### 6. Critical review (codex gpt-6.1-sol, read-only), corrections taken
- The junk 0xbf4f7906bf151f5d is the *function address* read from
  [[x19]+0x78], not the object's first word: [x19] was a readable pointer.
  As two floats it is about (-0.583, -0.810): unit-vector-like data, so the
  light's memory most likely now holds another object (e.g. an array of
  vectors) - reuse, not a random overwrite.
- "framebuffer 11 made" was at ~45 s (before frame 2100), the crash at
  207 s: not connected. Dropped.
- The game has its own allocator, so "hardened_malloc exposed it" has no
  evidence. Dropped as a lead.
- New concrete suspects in our code: (a) sound: the requested frame count
  per game callback has no maximum (iOS caps it, usually 1156, 4096 when
  locked), and AudioUnitSetProperty ignores unknown ids (incl. maximum
  frames per slice); a game callback with fixed scratch space would
  overrun its own heap. (b) callback replacement doesn't wait for a
  running copy. (c) dispatch_set_target_queue is ignored. (d) Objective-C
  atomic property accessors aren't atomic.
- Added one-time DIAG log lines for (a), (b), (c) (temporary).

### 7. Longer runs (5 min), taps only
- Pixel no-window run 1 (300 s): no crash. Emulator window (330 s): no crash.
- But the fight stops at the tutorial "SWIPE TO STRIKE" and waits: taps
  never advance it, so these runs barely played. Weak tests; runs 2-3 on
  the Pixel stopped.
- Added swipes to IOS_TAPS (`t:x,y>x2,y2`) and to the emulator script
  (adb input swipe); new 5-minute runs dodge, strike both ways and block.

### 8. DIAG results, emulator window run (swipes)
- Sound: the game sets only property 11 (element count 64), 2 (output
  sample rate) and 8 (stream format per bus); never 14 (maximum frames per
  slice), so it relies on iOS's default. AAudio asks 960 frames on the
  emulator; the biggest request to one game callback so far is 896 frames
  (bus 54, 32000 Hz, playback speed 1.4). Under 1156 here; the Pixel's
  AAudio size is still to measure.
- dispatch_set_target_queue: called twice, both from SDK disk-cache code
  (-[... initWithCacheFolder:] at 0x10004ceec and an -init at 0x1000608dc),
  not render code. Not this crash; still a real gap (to fix: honour
  serial target queues).

### 9. Emulator window, owner's save, 5 min with swipes/dodges/blocks
- Won the second fight, reached the level-up screen (331 s, 19200 frames).
  No crash. Biggest sound request still 896 frames.
- Conclusion: the save-to-second-fight path doesn't crash the emulator even
  with full play. Next: the owner's whole path from a fresh start (intro,
  first fight, movie, walk, second fight) in one run.

### 10. Pixel no-window, owner's save, 5 min with swipes (run 1 of 3)
- Real play: won the second fight and was deep in a later fight (enemy at
  496 health) at 300 s. No crash.
- Sound in no-window runs is pulled in tiny pieces: biggest request to a
  game callback 91 frames (vs 896 in the emulator window with AAudio
  960). So no-window runs never test large sound requests; only the
  window (AAudio) path does. The Pixel's AAudio size is still unmeasured.
- Run 2 of 3: no crash either.

### 11. Emulator window, fresh start (owner's whole path), 10.5 min
- Both videos, the opening scene, the first fight; random taps then
  opened the item menu after it (may not have reached the second fight).
  No crash. Biggest sound request 896 frames (speed up to 2.0).
- Tally so far: 0 crashes in ~40 min of play across emulator window and
  Pixel no-window runs. The only crash is the owner's Pixel window run.
  Next evidence needs the Pixel window (asked the owner).
- Pixel no-window run 3: no crash (3 of 3 with real fighting).

### 12. Fixed the gaps found on the way (general, not this crash for sure)
- Sound: one render call never asks a game input for more than the
  unit's maximum frames per slice (iOS default 1156; property 14 now
  set/get). Bigger needs are split into several calls, like iOS. Test:
  mixer test asserts the most frames per call <= 1156 (it needed ~2200).
- dispatch_set_target_queue: a queue targeting a serial (or the main)
  queue now runs its blocks there, in turn with that queue's own blocks;
  dispatch_get_current_queue still names the queue the block was sent to;
  dispatch_sync deadlock check uses the real runner. Test:
  queues_sharing_a_serial_target_take_turns (no overlap, order kept).
- objc_copyStruct with atomic=true now locks (64 address-picked locks, as
  Apple). Pointer get/set are single 8-byte accesses (already atomic on
  ARM64) and objects are never freed, so they stay as they are.

### 13. Screen-size runs (Pixel, no window, 4 sizes x 90 s) and the window test
- Side result of plan item 2 (job 2EB): 568x320@2, 960x540@2, 1212x540@2,
  909x405@2, each from the owner's save into the fight: 0 crashes.
- Window test (owner said yes, muted): ~/.cache/ios-debug/phone-window.sh,
  Game Bridge window on the Pixel, copy of the owner's live save (backup in
  ~/.cache/ios-debug/owner-save-backup-*), 3 runs x 7 min of taps/swipes;
  the owner's save is put back after. Predicted: a crash here and never in
  no-window runs = the window path (AAudio, real surface, movie) matters;
  0 crashes again = rarer than ~1 in 25 min, look harder at the code.
- Result (job 2EE): run 1 ~4.5 min, ended when the owner switched apps
  (Android killed the :ios process, not a crash); run 2 7.4 min, no crash;
  run 3 CRASHED at start-up (during the startup movie, ~10 s in):
  signal 11 at 0x100271cc0 `ldr x8,[x8,#0x10]` with x8=0, on a game thread
  whose entry is 0x100929dd0. The crash report's memory dump of the same
  object (x22=0xc002094e06d0) shows a valid vtable 0x...989b5160 a moment
  later: the object was written, but this thread read it too early.

### 14. Read the crash site: Unreal's render-command ring buffer
- 0x100271c38 is the rendering thread's loop: `BeginRead(&cmd,&size)`
  (0x100092a8c), `cmd->Execute()` (vtable +0x10), `cmd->~()` (vtable +0),
  `FinishRead(size)`. The owner's crash (attempt 3) was also a render
  command's Execute reading junk (UpdateLightTransform).
- BeginRead: `ldr x9,[x0,#0x10]` (write position), `dsb st`, `ldr
  [x0,#0x20]` (read position), then the caller reads the command. `dsb st`
  only orders stores, so the core may load the command before the write
  position: the reader sees a half-written command. The whole program has
  26 barriers, all `dsb st`, and no others.
- Three agreeing signs: (1) the field read as 0 at the crash but valid in
  the dump right after; (2) the read side uses a store-only barrier; (3)
  only the Pixel (weakly ordered ARM cores) crashes, never the emulator
  (x86, which keeps load order), and only rarely, in render commands.
- Fix (general, every game): the loader turns store-only DSB/DMB barriers
  in instruction sections into full ones (`dsb st` -> `dsb sy`, `dmb
  ishst` -> `dmb ish`, ...). Encodings checked with llvm-mc. A full
  barrier is never wrong in place of a weaker one. Test:
  store_only_barriers_become_full_barriers.
- Proof still to do: window runs on the Pixel, 3 loads + 30 min, no crash.

### 15. Proof: Pixel, Game Bridge window, sound muted, 3 loads x 10.5 min
- Script: ~/.cache/ios-debug/phone-window.sh 3 with SECS=630 TAG=proofC
  (owner's save copied in fresh each run, walk into the second fight, then
  dodge/swipe/block/tap inputs the whole time; owner's save put back after).
- Predicted: with the barrier fix, no crash in 3 loads and 30+ min (before
  the fix the owner crashed at 207 s, and attempts 10-13 crashed within a
  few minutes in some runs).
- Actual (2026-10-18, commit 6ae0c4a + e54f6dd): run 1, 2, 3 each reached
  frame 37800 at 631 s, no crash, no STOP line (the only "crash" text is the
  game's own crash reporter being refused task_threads at start-up). 31.5
  minutes of play in total, ~60 fps (300 frames per 5.0 s, except the
  first 14 s of loading and one 7.8 s stretch per run at ~8-9 min).
  Earlier with the same fix: 5.2 min clean (proofA), ~10 min (proofB).
- Conclusion: fixed. Plan item 1 done.

## Final entry (2026-10-18)
- Root cause: the game's rendering thread reads Unreal's render-command
  ring buffer after a store-only barrier (`dsb st`), which doesn't order
  loads; the Pixel's weakly ordered cores could read a command before it
  was fully written, so a render command ran with junk in it. The x86
  emulator keeps load order, which is why it never crashed there.
- Fix: the loader makes every store-only DSB/DMB barrier in a game's
  instruction sections a full barrier (all games, commit 6ae0c4a).
- Evidence: attempt 14 (code read, three agreeing signs) and attempt 15
  (31.5 min and 3 loads clean on the Pixel window where it crashed before).
- Known risk left: FinishRead frees ring-buffer space with no barrier at all
  (codex review); no crash seen from it. Fixing it would need an
  engine-specific patch, which needs the owner's OK first.
- Temporary DIAG sound logging removed after the proof.

## Sub-agent reviews
- 2026-10-18: codex gpt-6.1-sol: see attempt 6. Took all corrections; its
  ranking (stale light pointer > sound callbacks > frame counts > target
  queues > atomic properties > PVRTC) is being tested with DIAG logs and
  window-mode runs.
- 2026-10-18: codex gpt-6.1-sol (attempt 14 theory): writer side is right
  (`dsb st` before publishing at 0x100092968/0x100092a08); reader-side
  reasoning is sound and `dsb sy` closes it. But the crashes don't prove
  it: the two crashes could have different causes (buffer reuse, more than
  one producer, early light destruction, a stand-in overwriting memory).
  FinishRead (0x100092aec) frees space with no barrier at all; only
  rewriting existing barriers can't fix that (left as a known risk: it
  would need an engine-specific patch). Took: bounds/alignment check on
  the rewritten sections. Proof = same Pixel window runs, clean.

---

# Debugging: Infinity Blade II aborts in its own memory manager (PC, step 3)

## Symptom
Expected: the game keeps loading after its graphics start.
Actual: `abort()` from the game's built-in memory manager (dlmalloc-style;
`SwordGame` 0x1009259e8 → abort at 0x100925b68) on a game-made thread
(likely its loader), while freeing. The check that fails: a free-list link
points below the lowest address the manager ever used, i.e. its records
were overwritten or two threads changed them at once.
How often: two of two "keep going" runs (2026-10-18), at different moments.
Reproduce: `IOS_RUN_SECONDS=300 ios/tools/run-pc.sh <IB2>.app --keep-going`.

## Suspects (ranked)
1. Our dispatch queues: serial queues ran blocks at the same time
   (`dispatch_async` started a thread per block) and `dispatch_sync` ran at
   once without waiting its turn. Code that uses a serial queue as a lock
   then races; C++ code in it allocates through the game's manager.
2. A stand-in writes past the end of a game buffer (struct sizes, strings).
3. Locks: Apple static mutex values or recursive types turned into host
   locks wrongly.

## Attempts
### 1. Read the game's code around the abort
- Where: llvm-objdump of the arm64 part, 0x100925940..0x100925b70
- Result: dlmalloc free path (`least_addr` checks), called through a wrapper
  that takes `pthread_mutex_lock` first (0x100975c48).
- Conclusion: the manager is locked by the game; corruption or a lock fault.

### 2. Checked stand-ins
- mutex static-value conversion, recursive type number, OSAtomic*, spin
  locks, mmap flag conversion, stat/dirent sizes, sysctl, getCString:
  all looked right.
- dispatch: serial queues were not serial, dispatch_sync did not wait.
  Fixed (suspect 1); result pending.

### 3. Stopped in gdb at abort() (qemu -g, `break abort`)
- The block being freed had its header overwritten with a repeating 8-byte
  value (0xfae9f686827d7d82), like texture pixels.
- Caller frames: the game's texture upload (glCompressedTexImage2D /
  glTexImage2D at 0x1009127c0/0x100912810), then freeing the pixel buffer.
- Queue fix (attempt 2) did not change the abort: same instruction, every run.

### 4. The game reads the GL extension list (wide strings in the program)
- It looks for " GL_EXT_texture_compression_s3tc ", "..._pvrtc ",
  "..._atitc ", " GL_OES_depth_texture ".
- We passed Mesa's list (has S3TC/DXT, no PVRTC). The game then took a
  path an iPhone never takes and wrote texture data past its buffer.
- Fix: report the iPhone 5s extension list, limited to what this layer or
  the chip can really do; PVRTC always (decoded here when the chip lacks
  it); compressed-format list and size limits as on the iPhone 5s.
- Result: strict run gets past the old abort point; next stop is an
  unrelated missing function (swscanf). Abort gone in 1 of 1 runs so far.

## Current best explanation (2026-10-18)
Root cause: our GL extension list described the PC's chip, not an iPhone,
so the game sized texture buffers for one format and filled them with
another. Confirmed by the abort going away once the list matched.

---

# Debugging: IB2 shows 2 frames, then no more (PC, step 3)

## Symptom
Strict run lasts 400 s with no stop, but only 2 frames appear (about 100 s
and 117 s). Frame 1 is empty; at frame 2 the bound draw framebuffer has a
huge name and is incomplete (0x8cd7), GL error 0x506.

## Attempts
### 1. Call chains of every thread at 165 s (IOS_STACKS_AT)
- Game thread: `appSleep` loop in the game's movie code,
  `while ([MovieHelper bIsPlaying]) appSleep()`. Render thread idle in
  `pthread_cond_timedwait` waiting for work.
- The game's KVO handler reads `change[NSKeyValueChangeNewKey]`; our
  constant's text was "NSKeyValueChangeNewKey" but our change dictionaries
  use "new" (Apple's real value), so it read nil → "Unknown movie state 0"
  and never started (or finished) the startup movie.
- Fix: Apple's real values for the KVO change keys and three NSError keys
  (NSLocalizedDescriptionKey was also mismatched with our NSError code).
- Result: the startup movie now "plays" (ends at once) and frames keep
  coming (21+ frames by 200 s instead of 2). Every frame still has GL
  error 0x506.

### 2. The huge framebuffer name
- Log: "framebuffer 3418899048 made" on the game thread (not the render
  thread): glGenFramebuffers ran with no EGL context current there, so the
  name is whatever was on the game's stack. Frame 2 binds that name.
- Added: log every setCurrentContext (first 40) and glGenFramebuffers with
  no context current (with the call chain).
- Chain: game thread → stats/HUD drawing → FCanvas::Flush → direct RHI
  path → RHISetRenderTarget → FES2FrameBuffer made. FCanvas::Flush only
  draws directly when IsInRenderingThread(), which compares
  `pthread_mach_thread_np(pthread_self())` with the number the game saved
  for its rendering thread from `pthread_mach_thread_np(render_thread)`.
- Our pthread_mach_thread_np ignored its argument and returned the
  caller's id, so the game thread saved its own id as the rendering
  thread's, and later believed it was the rendering thread.
- Fix: per-thread table (pthread_t → id), filled by every thread start
  before the game's code runs; pthread_create waits for it. Unit test.
  Result: pending.
