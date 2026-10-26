# Debugging: SpellCraft on the Pixel

SpellCraft is a 32-bit (armv7) iPhone game. It runs through the translator in
`ios/src/guest32`. Branch `ios/spellcraft` (worktree
`~/dev/arm32-bridge-wt/spellcraft`) is based on `owner-dp`: **never push it**.
Commits stay local.

Last updated 2026-10-26.

## Where it stands

- Plays from a new game through the tutorial into the first dungeon and fights,
  with the rat and the monster cards drawn.
- Hall screen: steady 60 fps.
- Hall → Dungeon screen change: the longest freeze is about 150 ms (126 and
  175 ms in two runs). It was 425 ms on build 338f403 and 324 ms at the start
  of 2026-10-25.
- Tests: 475 pass (`ios/tools/test-pc.sh`, runs under qemu).

## Owner's complaints and what happened to each

| Complaint | Status |
|---|---|
| Moving between screens isn't smooth | Much better (425 → ~150 ms). Still not iPhone-smooth. See "Open". |
| A chosen spell stays on screen too long | Not checked again since the speed work. Earlier measurement: casting froze 80–120 ms, about 60% of it software drawing of the monster card. |
| Taps feel slow | Not measured again since the speed work. |
| Map froze on the "The Cookery" box | Fixed (dc0f414, `game_fixes.rs`): after a skipped "show once" box, the tutorial goes on. |
| Face box froze, couldn't tap | Not reproduced. Seen once on the old build while the phone was out of memory: a tap landed on a half-built name box and opened a "Face" box, then the tutorial lock blocked it. Three clean runs with free memory. |
| "Worse than before" | Measured: the new build was not worse (hall 60.3 vs 58.4 fps, screen change 324 vs 425 ms). The phone was short on memory at the time (swap 96–99% full). |
| Unlock to 120 Hz | Left at 60 on purpose. See "120 Hz" below. |

## Fixes so far (newest first)

- 0134120: each picture file is read once (bytes kept until the first
  decode). `imageNamed:` answers known files from a listing of the app
  folder; a miss still checks the disk. Opaque pixels are copied byte for
  byte. Axis-aligned paint works per row and column. New labels draw on up
  to 4 threads.
- 27d1c42: dictionary string keys compare their text directly.
  `dispatch_sync` onto an idle serial queue runs on the caller's thread, as
  Apple's GCD does. Remembered host messages keep a CallPlan. Guest→host
  selectors are cached. `imp_for` uses one lock.
- f8f286b: framework (host) messages remember their target.
- d19f774: fast four-byte pixel read/write.
- e8a0c26: FxHash for the lookup tables.
- dc0f414: `imageNamed:` remembers every answer; the Cookery-box fix.
- 338f403: `renderInContext:` lays the view out first (the rat was invisible).
- 4e72da5: GCD jobs run on kept threads (screen changes froze ~1 s).
- 0ce71ca: repeated messages go straight to the game method.
- 116b29a: mimalloc for the bridge's own memory (GrapheneOS's
  hardened_malloc caused ~100 ms stalls).
- 89f33ce: OpenGL ES 1 uses the OES framebuffer functions (PowerVR).

Every independent review (Sol 6.1) finding was fixed before commit.

## 120 Hz

SpellCraft must stay at 60. `AEGameWorldView` always uses an
`AEPeriodicTimer` at 1/60 s (`useDisplayLink` is never set).
`_frameCountFromTimeInterval:` turns elapsed time into whole 1/60 s steps and
rounds up when the fraction is over 0.2. So a 120 Hz tick still runs a full
step each time, and the game would run at double speed.

The other 8 iPhone games on the phone already run at the screen's top rate:
their display link is unlocked in `fw/runloop.rs`, and the Unreal games use
`MaxSmoothedFrameRate` in `fw/paths.rs`. That comes from reading their code,
not from live play.

## Open (ranked)

1. **Screen change ~150 ms.** About 350 ms of game-thread work before the
   fixes. Biggest parts last time: bridge messages ~25%, CGContextDrawImage
   ~12%, pixel conversion ~8%, PNG decode ~6%, nib loading ~5%, forwarding
   ~5%. Profile it again before picking the next fix.
2. **`pictures::tick` tree walk.** ~1600 samples per 8 s: it walks
   layer_children/property_get 20 times a second.
3. **Spell cast freeze (80–120 ms)** and **dialog opening (217 + 158 ms
   gaps)**. Measure both again; E (faster drawing) may have helped the cast.
4. **Tap delay.** Never measured directly. lat.py's tap-dot detection didn't
   work; use logcat touch timestamps against the first changed frame.
5. **HUD refresh freeze in fights.** 40–60 ms every 0.5 s from
   `SCPlayerDisplayController periodicUpdateNotification:` (~700 messages each
   time). Messages are faster now; not measured again.
6. **Small leftovers:**
   - Stretchable/resizable image copies still re-read their file.
   - The first `imageNamed:` lists the whole app folder while holding a lock.
     Its cost on the phone's /sdcard is unmeasured.
   - `fw::sound::tests::music_player_keeps_time_loops_and_seeks` failed once
     under heavy load (0.207 vs 0.25). It passes alone and in every full run
     since. Fix it if it fails again.
7. **Tutorial hand offset.** This is the game's own wide-screen quirk. The
   owner said to leave it as on the iPhone.

## How to measure (phone: Pixel 10, serial 56280DLCR0026M)

Scripts are in `~/.cache/ios-debug/sc-phone/`:

- `start.sh`: starts the game muted. `FRESH=1` backs up the save to
  `/data/local/tmp/SpellCraft-home-HHMMSS` and starts a new game. `IOSENV=`
  sets test switches.
- `tap.sh x y [wait]`: taps the screen. Game points are
  ((x-253)/3.375, y/3.375).
- `stop.sh`: keeps the log, closes the game, puts the volume back.
- `hall.sh NAME`: new game to the hall, then video fps and game-thread
  samples (16000 = always busy).
- `trans.sh NAME`: hall → Dungeon tap, and the longest stills after it.
- Build and install: `tools/build-app.sh --install`.
- Profiling: `simpleperf record -p PID -g -f 2000` as su. Symbolize with the
  NDK 28 `report_sample.py`. Copy the unstripped .so into the symfs under
  the current `pm path` after each install.
- `screenrecord` frame gaps mislead on still screens. Compare runs of the
  same screen.

**Owner's real save:** `/data/local/tmp/SpellCraft-home-230909`. Put it back
after any FRESH run:

- Move the current home aside.
- `cp -a` the save into place.
- `chown -R u0_a299:u0_a299`.
- `restorecon -R`.

Longer notes and raw numbers: `~/.cache/ios-debug/spellcraft/DEBUGGING-clunky.md`.
