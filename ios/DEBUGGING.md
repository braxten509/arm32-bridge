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
