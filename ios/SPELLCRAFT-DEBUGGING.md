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
- Tests: 477 pass (`ios/tools/test-pc.sh`, runs under qemu): 474 regular
  plus 3 separately run ignored/GIL tests. Includes image-copy and warm
  exception-barrier regression checks. The image-fix ARM64 PC binary builds.
- Image-copy fix is installed and checked on the Pixel: both hall-to-dungeon
  runs show the expected pictures. Initial video gaps were 143 and 108 ms,
  versus 142, 125 and 131 ms in three matched baseline runs. No clear speed
  gain is established; the file-reuse correctness fix stands.
- The barrier-cache APK is installed and visually checked through map,
  encounter and the 19-HP rat fight. Repeated warm stub lookup is absent in
  two phone profiles. The HUD pause remains reproducible after a long card
  wait. Owner save hash is unchanged; game stopped and test settings cleared.
- PC map smoke check passes (`cap-copy-map-final`, job 07B): Play hits
  UIButton, the exploration screen appears, and the requested stop exits 0.
  Earlier fresh-game checks failed because Play was queued while the welcome
  card was still open. Sequence fresh-game taps on the prior tap's
  `screen drawn after tap at X,Y` line, not `ready`.

## Owner's complaints and what happened to each

| Complaint | Status |
|---|---|
| Moving between screens isn't smooth | Much better (425 → ~150 ms). Still not iPhone-smooth. See "Open". |
| A chosen spell stays on screen too long | Fresh selection recording has a 164.7 ms gap (game log 169.2 ms). Selection and casting are separate events; current selection profile is being analyzed. |
| Taps feel slow | Not measured again since the speed work. |
| Map froze on the "The Cookery" box | Fixed (dc0f414, `game_fixes.rs`): after a skipped "show once" box, the tutorial goes on. |
| Face box froze, couldn't tap | Not reproduced. Seen once on the old build while the phone was out of memory: a tap landed on a half-built name box and opened a "Face" box, then the tutorial lock blocked it. Three clean runs with free memory. |
| "Worse than before" | Measured: the new build was not worse (hall 60.3 vs 58.4 fps, screen change 324 vs 425 ms). The phone was short on memory at the time (swap 96–99% full). |
| Unlock to 120 Hz | Left at 60 on purpose. See "120 Hz" below. |

## Fixes so far (newest first)

- Uncommitted: exception barriers remember their process-lifetime personality
  stub address, avoiding a shared-table lock and name lookup on each call.
  The test failed at the recursive-lock guard before the fix and passes now;
  nested contexts and depth-dependent return traps are checked unchanged.
  All 477 PC checks pass. Sol 6.1 review (t52) found no blockers. Phone
  installed library hash is verified. Phone profile sees zero warm barrier
  stub lookups (old profile: 255 samples). No clear phone speed gain is
  established: short card-wait clips looked better, but matching the old
  card-picture buildup restored the half-second pauses. See HUD below.

- Uncommitted: stretchable/resizable image copies share their source file
  bytes (`Arc<Vec<u8>>`) and retain its decoded CGImage when available.
  They no longer need to re-read the source file. The new test deletes the
  file before copying and releases the source before decoding the copy,
  checking both decoded and undecoded sources. It failed before the fix,
  then passed; all 476 tests pass. Sol 6.1 review (t50) found no blocker.

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
   ~5%. These are from `tp1.data` on f8f286b, before the latest fixes.
   New current-build evidence is in
   `~/.cache/ios-debug/sc-phone/direct-20261026/`: three matched recordings,
   memory/thermal records, hall.data and transition.data. The installed
   library was matched byte-for-byte to its local symbols. Baseline initial
   video gaps: 142/125/131 ms; game log: 138.5/123.5/135.3 ms. The old C+E
   gain remains within noise. Review the new transition window before
   choosing the next speed fix. Filter samples by game-thread ID 17579;
   image workers also use the name Thread-1, so name-only totals mix them.
2. **`pictures::tick` tree walk.** ~1600 samples per 8 s: it walks
   layer_children/property_get 20 times a second. A read-only audit (t49)
   identified T1–T8: batch class checks, cache screen size, faster property
   maps, an empty-animation fast path, batch layer reads, fewer child-list
   copies, reuse text buffers, and track changed views. None is implemented
   or measured. Tracking changed views must also handle async image/text
   completion; it needs an audit of every writer and checks in other games.
3. **Spell selection, casting and dialog pauses.** Fresh selection video
   has a 164.7 ms gap; a separate current-build profile logs 154.2 ms. In
   its inferred 200 ms event window (game TID 20298), 47.4% of samples pass
   through renderInContext, 44.4% through Canvas::paint and 13.6% through
   PVRTC decode (inclusive categories overlap). Barrier work is only 1.16%.
   First-use versus repeat-selection cost is unresolved, as is the exact
   rendered layer. Old cast (80–120 ms) and dialog (217/158 ms) numbers are
   different events and need fresh checks; do not substitute selection data.
4. **Tap delay.** Never measured directly. lat.py's tap-dot detection didn't
   work; use logcat touch timestamps against the first changed frame.
5. **HUD refresh freeze in fights.** Fresh image-fix recordings confirm it:
   three 8-second idle rat fights have 15/12/16 gaps over 35 ms, with maxima
   57.7/59.6/56.8 ms. Game TID 19258, not the shared Thread-1 name. Review
   of 16 half-second bursts finds bridge leaf work 55.7% of sampled cycles,
   pictures 2.6%, drawing zero samples. Repeated barrier stub lookup is
   4.44% (4.03–4.26% with shifted/wider windows); the small local cache above
   targets this part only. It does not establish a fix for the whole pause.
   **New history confound:** on the same cached build, entering combat after
   ~210 card-back pictures yielded 1/0 gaps >35 ms (max 35.2/29.5 ms); waiting
   until 740 pictures (771 before exit) yielded 15/15 (max 67.3/67.5 ms). Both finish at 14 shown
   pictures with the same 19-HP rat. The aged profile (TID 23635) still has
   zero warm barrier stub lookups. Earlier activity matters; retained views,
   observers or another mechanism are not established. Compare navigation
   history and time in combat before claiming a speed improvement. CPU
   placement/frequency also differed, so sampled cycles are not CPU time.
   Independent review confirms comparable fight ages; aged-run pauses start
   1.3 seconds into combat. Next compare periodic-notification target IDs and
   messages per tick, plus reachable view-tree nodes after leaving the card.
   Fourteen output pictures do not prove that only fourteen objects remain.
6. **Small leftovers:**
   - Fixed locally: stretchable/resizable image copies reuse source bytes
     or decoded pixels (see above). Phone checks passed; no clear speed gain.
   - The first `imageNamed:` lists the whole app folder while holding a lock.
     Its cost on the phone's /sdcard is unmeasured.
   - `fw::sound::tests::music_player_keeps_time_loops_and_seeks` failed once
     under heavy load (0.207 vs 0.25). It passes alone and in every full run
     since. Fix it if it fails again.
7. **Tutorial hand offset.** This is the game's own wide-screen quirk. The
   owner said to leave it as on the iPhone.

## How to measure (phone: Pixel 10, serial 56280DLCR0026M)

The owner later explicitly authorized the main agent to use the phone
(2026-10-26: "No YOU"). Direct adb and root access work. Test from a
separate IOS_HOME and verify the owner save is unchanged afterward.
Leave this work uncommitted and never push this branch.

Current artifacts and helpers are in
`~/.cache/ios-debug/sc-phone/direct-20261026/`:

- `capture.py LABEL video|profile [tap-x tap-y]` records 8 seconds from an
  already verified screen. Use a new label every time.
- `barrier-setup.py` records the install/map-check recipe; its fixed home and
  file names have already been used. Pick new names before another run.
- `barrier-measure.py` captures two idle fights plus a profile, then stops
  the game, clears the test property, restores volume 0 and checks the save.
- Map readiness: require the new `app home folder:` line, then main menu,
  Play, then `screen shown: SCExplorationViewController`. A map does not emit
  the hall's GL frame-300 marker. Inspect screenshots before the next tap.
- Phone taps: Play 1940,170; third room 1500,260; card flip 1190,430;
  spell selection 1360,960; target 1680,440. Allow each screen to finish.
- Build: `tools/build-app.sh` through a tracked, time-limited systemd job.
  Install the built APK only after saving previous unstripped symbols.
- Profiling: `simpleperf record -p PID -g -f 4000 --duration 8` as su.
  Symbolize with NDK 28 `report_sample.py`. Use the exact game TID from its
  log, not Thread-1; workers share that name. Rebuild symfs paths after every
  install and verify the installed stripped library hash.
- Video gaps only measure stalls on an actively changing screen. Use
  `ffmpeg -fps_mode passthrough` when extracting frames.

**Owner save stays in place:**
`/data/data/dev.braxten.gamebridge/files/ios/SpellCraft/Documents/PlayerData.data`.
Its expected SHA256 is
`621a01aaa228ef9a890d1543f588f8bc3bcd467414d0e31f54bc2776d87d2a0d`.
Use a new isolated `IOS_HOME` copied from `sc-direct1` for map/fight checks;
`sc-hall-seed` is the separate hall seed. Preserve the existing test homes.
The old `/data/local/tmp/SpellCraft-home-230909` backup remains untouched.
Do not use the older `FRESH=1` helpers that move the owner's home.

Longer notes and raw numbers: `~/.cache/ios-debug/spellcraft/DEBUGGING-clunky.md`.

## Continuation 2026-10-26: card history and fight refresh

- Owner enabled away mode and explicitly said "Use my phone as need".
  Continue with isolated test homes; preserve the owner save and existing
  uncommitted fixes. No commits or pushes.
- Plan (no plan/todo tool is exposed):
  1. Completed: compare retained evidence; identify repeated portrait output
     and the layout re-add loop. Observer counts remain a fallback lead.
  2. Completed: failing-first layout and late-controller-link checks, then
     the narrow attachment fix preserving existing geometry behavior.
  3. Completed: first attachment guard passes 478 PC checks and review, but
     fails the real phone pause check; retained below as a narrow correctness fix.
  4. Completed: failing-first child-creation check, scoped implicit layout fix,
     479 passing PC checks and independent Sol code review.
  5. In progress: two matched phone recordings, final notes and phone cleanup.
- Passing check: two matched long-card-history runs improve recurring fight
  pauses beyond the existing 56–68 ms range, preserve the displayed fight,
  and leave the owner save hash unchanged. A build alone is not a pass.
- Decisions: investigate the last documented history confound first, rather
  than make broad picture-tree speed changes. Existing simpleperf recordings
  resolve functions but not notification target identities or live object
  counts; use focused diagnostics only if static inspection cannot settle it.
- Initial code observations (not a root-cause claim): NSObject's base
  retain/release are stand-ins; notification recipients are removed only by
  explicit removeObserver calls. UIView attachment removes existing parent
  membership before inserting, so ordinary re-adding does not duplicate an
  identical subview. DeepSeek audit t54 is checking these paths independently.
- Apple references checked: UIView addSubview establishes a strong reference
  and one superview (https://developer.apple.com/documentation/uikit/uiview/addsubview(_:));
  automatic notification observer cleanup is only promised for iOS 9+
  (https://developer.apple.com/documentation/foundation/notificationcenter/removeobserver(_:)-2yciv).
- Narrowed lead: in the saved card log, the increasing output is repeated
  copies of `23599-29.png` at the identical 67,44 / 154x222 rectangle. Reading
  that saved picture identifies the player's outfit portrait, not card backs.
  `UIView(AENibView) layoutContentView` at guest 0xd9b78 re-adds its existing
  content view on every layout. Bridge attachment unconditionally marks both
  views for another layout even when child is already last in that parent.
- Regression written first in uikit.rs: a layout callback re-adds its own
  top child, two layoutIfNeeded calls must run it once, and an explicit new
  request must still run it. Red run 08F started before production edits.
  Proposed change is only a no-op check for addSubview on the existing top
  child. Sol reviewer t55 checks semantics. It is not yet proven to explain
  the avatar buildup or HUD stalls; phone comparison will decide.
- Red 08F: layout callback ran twice instead of once. The first no-op guard
  passed (091). Sol t55 identified a legacy geometry regression: late-linked
  controller views still need correction on a repeated add. Added that case
  before fixing it; 094 failed with portrait 320x568 instead of 568x320.
  Extracted the existing correction unchanged and call it on both paths.
  The no-op regression also checks pending child layout and sibling reordering.
  Full PC suite 097 and final Sol review are running.
- New same-session baseline base1: isolated home sc-hist-base1, 836 pictures
  before card flip, then same 19-HP rat and 14 displayed pictures. Two idle
  recordings retained under direct-20261026/fight/hist-base1-idle*. Test game
  stopped, property cleared and owner save hash verified unchanged.
  Video gaps: 15/15 above 35 ms; maxima 74.62/74.86 ms (the earlier matched
  profile had 67.28/67.48). Card snapshot to flip was 62.95 s; fight readiness
  to first recording was 21.97 s. New helper history.py matches these ages
  by wall time rather than requiring the fixed build to accumulate pictures.
- Sol t55 final code review: no blockers; extracted geometry helper preserves
  late links, real resizing, pending child layout and lower-child reordering.
  Build 098 is running. No broad notification, ownership or drawing changes.
- Full PC 097 passes: 475 regular + 3 ignored/GIL = 478 tests. Installed
  baseline library hash matches before-stripped.so exactly:
  2c40e4bc84ad8381fe5f5cc47125d88e4714fc778a0703a2e3a6675cc696fa98.
  Before-build APK and unstripped symbols are preserved in history-20261026.
- Build 098 and installation 099 passed. Installed new stripped library
  SHA256 18328eaea8545c323b374cb29d9b0de5f598cac72ef17050302c04550db12c30
  matches the saved layout-stripped.so; layout-symbols.so and layout.apk are
  preserved alongside it. First fixed phone check uses sc-hist-fix1.
- DeepSeek t54: general object lifetime stand-ins can retain observer/timer
  work; this is a candidate, not measured causality. Its statement ruling out
  autorelease is too broad: draining counted objects does not free uncounted
  NSObject instances. No lifetime overhaul made. Follow-up audits the actual
  repeated portrait creation instead.
- REA evidence ev_31ebb7e16cb0113d27ea8dc8a21fb1d48c6790c03c3cfe0877e865c7ca71be5d
  corroborates layoutContentView's re-add. Ghidra target is the existing thin
  sc-armv7 (sha256 4d5aa9f6788f3df9a72a89894783a91940fda72245c4e077ebed92520a0ff1d4).

- First guard phone result fix1: card still grows to 974 pictures; fight returns
  to 14. Gaps >35 ms are 16/15; maxima 75.84/66.70 ms. compare.py correctly
  fails its recurring-pause assertion. Capture 09E cleared the property and
  verified the owner save unchanged. No speed improvement claimed.
- REA ev_659d2fb6774d913b37ef2dda348988f87b18abec65d05184a236f8d2be0d6c2c:
  SCDungeonViewController viewDidLayoutSubviews (0x16c10) allocates a fresh
  SCPlayerView and inserts it into the controller root on each callback.
  Bridge attachment then schedules that same root again. This explains both
  new portrait identities and the failure of the unchanged-child guard.
- Next check before implementation: controller layout inserts a new child;
  repeated layoutIfNeeded must create one child, lay that child out, preserve
  explicit setNeedsLayout from inside the callback, and still respond to an
  external hierarchy/size change. Only uikit.rs and this record may change;
  earlier passed changes, object ownership, notifications and rendering are
  off limits. Decision: suppress implicit requests on an active layout subtree,
  not explicit requests. Track active state with a scoped reset.
- Supporting independent implementation: Microsoft WinObjC CALayer.mm
  _setShouldLayout ignores didLayout, whereas setNeedsLayout is unconditional.
  https://github.com/microsoft/WinObjC/blob/develop/Frameworks/QuartzCore/CALayer.mm
  This supports the distinction, not proof of Apple's precise internal code.

- Red 0A0 reproduces the new-view loop: two layoutIfNeeded calls create two
  children, expected one. Sol t55 reviewed the proposed active-layout guard:
  mark only views whose callbacks actually run (not clean traversed parents),
  check active before consuming dirty state, and preserve explicit requests
  during nested layoutIfNeeded. Added both boundary cases to the regression
  before production changes; expanded red run 0A1 is running.
- WinObjC's addSublayer/insertSublayer and bounds-size change call the guarded
  _setShouldLayout; this independently supports guarding both attachment and
  resizing. Implementation will use per-view scoped active state, no global
  cap, no game name checks, and no change to explicit setNeedsLayout.

- Expanded red 0A1 fails the same two-vs-one child assertion. Implemented
  ActiveLayout scoped state only for a view that actually receives callbacks,
  lasting through its descendant traversal. Nested layoutIfNeeded checks
  active before consuming needs_layout. Implicit attach/detach/resize requests
  skip active views; explicit requests remain unconditional. Clean parents
  visited solely for a dirty child stay eligible. Full checks 0A2, phone build
  0A3 and Sol final review run before installation.

- Scoped fix full PC 0A2 passes: 476 regular plus 3 ignored/GIL checks, 479
  total. Phone build 0A3 passes. Sol t55 final code review has no blockers;
  exact Apple internals remain unproven. Saved scoped APK and symbols before
  install; stripped library SHA256
  8b97be9ce72dbd67d2c6d0f5c0a15c1294ddc3726c25024462b0f297b31b7311.
- DeepSeek's follow-up audit missed SCDungeonViewController viewDidLayoutSubviews
  and incorrectly generalized its bounded search to all layout paths. Requested
  a focused correction at 0x16c10. Its useful negative findings about cached
  compositeImage and refreshCharacterImage do not rule out controller creation.

- Sol t55 recomputed raw timestamps: parser and gap counts match exactly. It
  caught age mismatch: base1 captures start 22/38 s after fight readiness, fix1
  starts 72/87 s. Card waits were 62.95/65.77 s. These clips establish persistent
  first-guard pauses, not a reliable improvement/regression estimate. Video
  gaps include recorder effects. Next phone sequence chains fight + capture
  automatically so the first recording starts at the intended 22 s; capture
  both clips on an active 19-HP rat fight and retain actual timestamps.

- DeepSeek t54 correction: confirmed SCDungeonViewController
  viewDidLayoutSubviews (0x16c10) makes a new SCPlayerView via viewWithFrame:
  (nib load each time) and inserts it below playerPosition without removing
  the old one. So every real layout pass of the root adds one portrait; the
  host loop multiplied that. Scoped fix (fix2) should bound it to the number
  of genuine layout passes.
- fix2 phone run: menu 9 pictures, map 25 (same as fix1). Card 0A7 next, then
  fight + capture chained in one job so recording starts at 22 s.

- RESULT fix2 (scoped ActiveLayout, stripped lib 8b97be9c...): same save,
  card wait 65.7 s (base1 63.0, fix1 65.8). Pictures before flip 27 (base1
  820, fix1 953): portrait buildup gone. Fight recordings start 26 s after
  fight readiness (base1 22/38): 0 gaps >35 ms in both clips (base1 15/15),
  max 27.7/28.0 ms (base1 74.6/74.9), p99 25.0/27.3 ms (base1 58.0/56.9),
  505/500 frames in 8 s (base1 468/471). Screens match base1 (portrait, card,
  19-HP rat). Root cause: host marked the root for layout whenever a view was
  attached during its own layout; SCDungeonViewController viewDidLayoutSubviews
  adds a new nib-loaded SCPlayerView each pass, so the loop re-ran every frame
  and stacked portraits; each fight HUD tick then walked the bloated tree.
  Committed locally on ios/spellcraft (not pushed).

## Continuation 2026-10-26 (later): 120 Hz and smoother screens

Owner, after the fight fix: "Fights are much much better! However, the
screen transition is still a little clunky. Also, I need you to update the
game to 120hz ... you have permission to alter the game to make it run more
like a modern one. A lot of clunkiness could just be the game, so please
update anything like that. The phone won't be available." Away mode: no
questions, no phone; decisions logged here.

Findings:
- Our UIView animations never moved: animateWithDuration and begin/commit
  applied changes at once (completion next turn). Every dialog, panel, card
  drag and popup in SpellCraft jumped. (uikit.rs "block animations".)
- UIKit pictures go to the phone at most 20 times a second (pictures::tick
  50 ms gate); Core Animation values are sampled only then.
- IosPicturesView decodes new pictures on the phone's UI thread inside
  show(), and drops every bitmap not in the current list.
- Opus t56 RE (notes and scripts: ~/.cache/ios-debug/spellcraft/re-120hz/):
  the real game loop is -[SCRenderer renderLoop] (0x34f10), AEPeriodicTimer
  1/60 (1/30 while UITrackingRunLoopMode), whole 1/60 frames, stall cap 4,
  draw then updateContent. AEGameWorldView is unused (earlier "120 Hz"
  note was about the wrong class). AEAnimationPlayer tick:/render:
  (0xbe528/0xbe5b4) use integer frames; aecAnimation::nodeTransform
  (0xca12c) interpolates keyframes linearly, so sampling at frame+0.5 is
  real 120 Hz motion. Main screen switches are 0.0 s cross-dissolve cuts
  (-[AERootViewController setChildViewController:] 0xda8da), also on iPhone.
  Dialogs/panels: UIView animations 0.2-0.5 s. SCAnimatedImageView
  flipbooks: 30 Hz NSTimer, imageWithContentsOfFile: every frame.

Plan (each step: failing test first, then code; all PC checks must pass):
- P1 UIView animations animate (center/frame place, transform, alpha) via
  Core Animation copies; completion/did-stop at the end. Sizes still jump.
  Check: view_animations_show_the_way_and_finish_at_the_end. DONE, 480/480.
- P2 While animations run, each list also carries the coming frames (one
  screen refresh apart, until the animations end, capped), with
  CLOCK_MONOTONIC times; the phone draws the frame for each refresh
  (Choreographer) so motion is screen-rate even when the game thread is
  busy. Check: moving_views_send_their_coming_frames + a JVM test of the
  phone-side parser/selector if feasible; PC run lists show the sections.
- P3 Phone decodes new pictures off the UI thread and swaps the list when
  ready; recently used bitmaps kept (bounded). Check: Gradle build; JVM test.
- P4A Replace -[SCRenderer renderLoop]: screen-rate tick, fixed 1/60 steps
  from an accumulator, stall cap kept, never updateContent with 0 frames.
  Check: test of the step accumulator; PC run counts updateContent steps per
  second (~60) while ticks run at the screen rate.
- P4B Half-frame drawing: sample aecAnimation::nodeTransform at frame+frac
  for players that moved. Needs a guest-function replacement hook. Check:
  unit test of the hook + PC run.
- P5 Modernize (owner permission): screen switches get a short cross-fade
  (0.2 s) instead of the 0.0 s cut. Switch IOS_SPELLCRAFT_FADE=0 to undo.
- P6 Flipbook frames: imageWithContentsOfFile: reads from a bounded cache.
Off limits: the fight/layout fix (6309128), owner save, the phone.

Work log and decisions (away mode):
- Sol review out (Codex usage limit until 6:25 AM): Opus t58 reviewed the
  plan instead. Its 12 findings: fixed #1 (resizes slid), #3 (ignoring
  touches), #4 (blocks per thread), #5 (settle), #6 (transitionWithView),
  #9 (faster ticks while a finger is down or a change is new); #2 limited
  with only_made (groups not re-drawn per sample); #7/#8 cost accepted with
  frame reuse (Sent::holds); #10/#11 (P4 risk) answered by the checks
  below; #12 done phone-side.
- P2: lists carry `@\t<ns>` frames for 0.3 s ahead on the refresh grid;
  the phone draws the frame for each Choreographer refresh. P3: phone
  decodes on its own thread, keeps 96 MB of unused bitmaps (LRU). P5: the
  Rust side marks instant screen cuts (`!\tfade\t250\tover|under`), the
  phone cross-fades 0.25 s (under the new screen when GL shows, else over).
  JVM check android/tools/check-picture-list.sh passes.
- P4A: -[SCRenderer renderLoop] replaced (game_fixes.rs render_loop):
  ticks at the screen rate, 1/60 s steps from carried time (stall cap 4.2
  steps: one step, no catch-up, as the game). Steps run, then the draw.
  IOS_SPELLCRAFT_HZ=60 keeps the game's own loop.
- P4B: the iPhone stand-ins sit at 0xD0000000, out of Thumb BL reach, so
  the nodeTransform call in -[AEAnimationPlayer render:] (+0x26a) now goes
  to a 12-byte jump (ldr.w ip,[pc,#4]; bx ip; nop; stand-in) written into
  the zero padding after the program's load commands (checked zero, before
  the first __TEXT section; the call's bytes and target are checked first;
  nothing changes if any check fails). The stand-in calls the real
  nodeTransform at the frame and, for an animation whose frame moved with
  the last steps, at frame+1 too, and moves the 6 numbers (x, y, scale x/y,
  turn in degrees, opacity: pushTransform) that fraction of a step on. Jumps
  over 64 points / 1 scale / 90 degrees (cuts, loop restarts) are not drawn
  between. Ticks with no step draw again (renderView; frameCount stays >= 1,
  since renderView runs updateContent itself at 0). Draw methods only refresh
  caches (xrefs: no tick/update in render paths), so extra draws do not move
  the game. Switch IOS_SPELLCRAFT_HALF=0.
- P6 decision: not done. File pictures go to the phone by path and the
  phone's bitmap cache (P3) already covers decoding; the Rust side only
  reads sizes. Revisit only if a PC profile shows imageWithContentsOfFile:.
- CORRECTION (PC trace, IOS_TRACE_MESSAGES=ransition + a log in
  controller_transition): screen switches are NOT 0 s cuts. The game sets
  -[AEApplication setViewTransitionDuration:] at start, and
  setChildViewController: calls transitionFromViewController with
  duration 0.5 and options 0x500000 (UIViewAnimationOptionTransition-
  CrossDissolve). Our layer ignored the transition kind, so the switch
  jumped: that was the clunky screen change. Fix (all games, iPhone-
  faithful): uikit::transition_fade(duration, options) -> cross_fade(s);
  the next list carries `!\tfade\t500\t...` and the phone dissolves.
  Also for transitionWithView: and transitionFromView:toView:. Flips and
  curls dissolve too (not drawn). The SpellCraft-only 0.25 s "cut fade"
  (P5, IOS_SPELLCRAFT_FADE) was based on the wrong reading and is removed.
  Test: transitions_fade_for_the_time_the_game_asks (failed first).
- Emulator (ios_bridge_test, x86_64 + ARM translation, 60 Hz): menus,
  the welcome box fade-out and the tutorial pointer work; the game world
  crashes in the emulator's own GLES1 driver (SIGSEGV in
  GLEncoder::s_glIsEnabled via libndk_translation) on the first GL frame,
  ALSO with IOS_SPELLCRAFT_HZ=60 (game's own loop, no half draws), so
  not caused by this work. Game-world checks stay on the PC (qemu,
  Mesa) and the owner's phone.
- Test flakes seen only while an APK build loaded the CPU:
  moving_views_send_their_coming_frames (fixed: the animation now begins
  at a set time) and fw::sound music_player_keeps_time_loops_and_seeks
  (unchanged file; passed on the rerun).
- Opus review t59 (Sol still out of usage): nothing serious in the patch
  core (BL/BLX fields, jump bytes, slide, Steps checked by hand). Fixed:
  #3 a fade left waiting by a transition that changed nothing shown now
  expires with the transition (Instant); #4 draw-ahead state is set right
  before renderView, after updateContent (draw()); #5 IosPicturesView
  drops lists after its decoder shut down. Shown not to apply: #1
  nodeTransform is void (r0 overwritten by `movs r0,#9` right after the
  call); #2 nodeTransform clamps to its first/last keyframe (ca152/ca172),
  so frame+1 past the end is safe. Accepted: paused players sharing
  animation data may twitch half a frame once; ticks stay at the screen
  rate while scrolling.
- Also fixed after the first PC runs: AEGLAnimationView draws animation
  players on its own timer, outside the render loop; HalfSteps now draws
  ahead only during the render loop's own draw (test extended first).
- PC fight check (FROM_HOME=cap-copy-map-final; taps: menu 499,53;
  `every=screen shown: SCExplorationViewController=>settled 367,77`;
  `every=screen drawn after tap at 367,77=>settled 281,127`): reaches
  SCCombatViewController; "first animation placed through the in-between
  drawing"; 4849-4887 of the first 5000 animation places drawn part of a
  step ahead (monsters' idle loops). Note the screen is 568 points wide,
  not 480 (phone x = 241 + 3.375 * points).
- A 266 s main-thread stall in the first fight run was the PC-only frame
  save (IOS_FRAMES_DIR, frame 300: glReadPixels + our PNG encoder under
  qemu): runs with in-between drawing off stalled at frame ~297-299 too,
  and with FRAME_EVERY=100000 (everything on) the fight ran 867 frames,
  no stall, exit 0. Not a phone issue. Use FRAME_EVERY=100000 for fights.
- Emulator splash -> menu still cut after review fix #3: the fade expired
  after its own 0.5 s while the slow emulator made the menu's pictures.
  Now a waiting fade is kept up to 2 s and always plays its full length.

## 2026-10-26: heart, spell card, spell panel slide

- Monster heart stayed full: its spec observes
  `parent.combatController.monsterHealth`, and our KVO only told observers
  about the first key of a path. Now (fw/object.rs) each observer keeps the
  objects along its path; a change of any key on that chain is told as a
  change of the whole path, and the chain is followed again afterwards
  (Apple's behaviour). PC fight log: "KVO: a change of monsterHealth along an
  observed key path (parent.combatController.monsterHealth)". All games.
- Spell card on the monster: the game's own design.
  `buildCardTexturesWithEncounterView:andSpellView:` places the spell card at
  the encounter card's spot, half size. `spellWasCast` keeps it while the
  spell has uses left, and plays `playSpellCardDeathAnimation` when it is
  used up.
- Spell panel slide (`SCTabbedPanelView animateIn/animateOut`, a 0.2 s
  UIView animation of center) felt clunky: our animations began when they
  were added, so the rest of a busy turn used up part of the slide before
  its first frame. Now, as on Apple, animations without their own begin
  time begin at the end of the main loop's turn (`animation::commit`, from
  run_loop_until before pictures::tick). One UIView block's animations
  share one start (ViewTiming.asked). The log line "animations begin at the
  end of their turn, N ms after they were added" shows the gap (84 ms in a
  PC fight).
- Music test flake fixed: the test's pretend sound card retries a turn the
  decoder held the queue for.
- Spell panel, second fix: the phone showed lists arriving ~250 ms late
  because pictures were made after the commit. Now pictures::tick makes the
  pictures for uncommitted animations first, then commits.

## 2026-10-26 (later): tutorial gap, drawer, tab-switch lag

- Tutorial gap: -beginStageInteraction performs -lockControlsAndPlacePointer
  2 s later; until then the store tab's other tabs were tappable. Fix
  (game_fixes.rs after_stage_interaction): while that perform waits, every
  control is off except the step's own (the focus control, found as
  -applyControlFocus finds it: currentStage focusControl key path on the
  current screen). Owner then had to tap the hat 3 times (all controls were
  off in the first version) - fixed by leaving the focus control on.
- Drawer in front of the tutorial box: the box opens in viewWillAppear, the
  drawer was added in viewDidLayoutSubviews afterwards. Now a screen is laid
  out before viewWillAppear (uikit.rs controller_transition, did_appear).
- Tab-switch lag (owner: "switching drawer tabs is a bit laggy"). simpleperf
  on the phone (25 s, 2000 Hz, symbols from target/aarch64-linux-android
  release lib via --symfs) showed ~0.5 s game-thread stalls per switch:
  (1) compose_groups ~300 ms: the drawer content panel is masksToBounds +
  cornerRadius 8, so every content change redrew the whole panel on the CPU
  (PC: 355x150 pt group, 14 pictures, 645 ms); (2) the game's own
  CGContextDrawImage calls ~200 ms (draw_bitmap per-pixel closure, clip
  masks with 4x4 supersampling).
  Fix (1): round corners alone are no longer composited. Each picture
  carries a `Corner` (rounded rect, screen points); the list line gets 22
  numbers; the phone clips with Path.addRoundRect (Core Animation's way).
  Groups still made for masks, shadows, turned bounds and round-in-round;
  inside them compose.rs honours corners (both radii) and leaves the
  group's own corner for its finished picture (after the shadow).
  Fix (2) (sc-draw worktree, DeepSeek): draw_bitmap fast path for straight
  transforms (precomputed bilinear rows/columns, fixed-point blend) and an
  analytic rectangle clip: ~6x faster in a qemu bench.
  Review (GPT-6.1 Sol) found the min-radius and shadow-cut bugs (fixed, test
  added); fixed point can differ by 2/255 from the general path at partial
  alpha (left).
- Garden flicker (going to the garden, picking a plant): the owner's 180 s
  phone recording (~/.cache/ios-debug/spellcraft/garden/sc-garden.mp4,
  scanned frame by frame for 1-3 frame bursts) shows none: the garden fades
  in smoothly at 3.0 s (one 50 ms pause). Ask for a new recording if seen again.
- The bad %@ ("buy_%@"/"use_%@" get 0xd0000048, a return-trap slot) only
  feeds Flurry event names (offline); left. Likely the same cause as below.

## 2026-10-26 (evening): stuck at the plant step (bridge, all 32-bit games)

- Owner: can't tap the plant the tutorial asks for. PC replay (phone save
  phone-save4) ended in a stop: `addObject:` on a stale return-trap word
  (0xd0000020). Cause: -[SCAlertController queueAlertWithFormatNamed:
  withTarget:andAction:] takes nil-terminated extra arguments; the bridge
  ran every game method as a nested call copying only the words the
  method's types name, so the extras (on the caller's stack) were lost
  and the method read stack garbage until it hit a trap address.
- Fix (owner picked "jump straight in, like Apple"): objc_msgSend and its
  _stret/Super2 forms no longer call a plain game method; they set r0-r3
  and jump into it (runtime Thread.jump_to, honoured by the translator and
  Unicorn after a stand-in returns Ret::Jumped), lr and sp left as the
  caller's: a tail call, as Apple's objc_msgSend. Still nested: KVO-
  observed setters (will/did change around the call), game_fixes methods
  (after_message runs after), forwarding, host methods, and every message
  host code sends. Test: guest32 tests game_methods_get_the_variable_
  arguments_their_caller_passed (failed with 1 instead of 4321 before).

## 2026-10-26 (evening): sideways "Not Ready to Harvest" box (old-game window)

- Cause: SpellCraft (iOS 6 SDK) shows AECustomAlertView by adding a dim
  view the window's size to the window and turning the box itself by
  statusBarOrientation (CGAffineTransformMakeRotation(M_PI_2) for
  LandscapeRight), as iOS 6 needed: the window stayed portrait there. Our
  layer forced old games' windows to landscape, so the box was turned twice.
- Fix (owner picked "full iOS 6 window model"): for apps built with the
  iOS 7 SDK or older, the window stays upright (portrait, as UIScreen
  reports) and the screen turns it (uikit::screen_turn / window_turn);
  root and presented controller views and our host alerts get iOS 6's
  placement (landscape bounds, centred, turned back: place_upright);
  pictures start from the window turn; touches come in screen points and
  go into window points. Hit-testing, locationInView: and swipe
  directions now follow each view's whole transform (all games).
- PC checks: 494 tests pass; Dungeon Plunder menu picture lists identical
  before/after; TownCraft Select Avatar identical; Monster Life unchanged.
  Not done on the PC: the box itself showing upright (the replay didn't
  reach it before its time limit); owner tests on the phone.
- Known gap (not in this change): the GL picture of a GL view placed
  straight in an upright window is not turned (Monster Life draws into a
  640x1136 buffer and looks sideways, before and after this change).

## 2026-10-26 (evening): screen-switch flicker, faded drawer, Dungeon Plunder walk stutter

- Flicker (owner video flicker2/flicker.mp4): during SpellCraft's 0.5 s
  cross-dissolve two SCRenderers draw into the same GL view and each
  presented; every eglSwapBuffers reached the screen, so it alternated
  old/new frame by frame. iOS shows only the newest frame per refresh.
  Fix: fw/frame_slots.rs: a main-thread frame waits half a refresh; a
  newer one drawn meanwhile replaces it; the run loop swaps it when due.
  Test: only_the_newest_of_close_frames_reaches_the_screen.
- Faded drawer: disabled buttons (tutorial disables tabs) were drawn at
  alpha 0.5. Apple draws them darker. Fix in pictures.rs `shaded`.
  Test: pressed_and_disabled_buttons_darken.
- DP walk stutter (owner video ~/Videos/bridge-recordings/
  dp-walk-152452.mp4, measured with ~/.cache/ios-debug/dp-walk/tools/
  shift.py, phase correlation per frame): while walking the map moves an
  even ~4.9 px a frame, but about every 25 refreshes (~5 times a second)
  one or two frames don't move and the next step is normal size: the game
  made ~115 frames a second for a 120 Hz screen. Cause: CADisplayLink was
  our own run loop timer at 1/120 s; a late call reset it to "now + 1/120"
  (lost phase), and timestamp returned "now". Dungeon Plunder is cocos2d
  (CCDirectorDisplayLink, calculateDeltaTime with gettimeofday).
  Fix: fw/vsync.rs: a helper thread hears Android's Choreographer (each
  refresh's time and the refresh period); display links fire on the next
  refresh, timestamp is the refresh's time, targetTimestamp the next;
  other repeating timers keep Apple's rule (stay on their own firing
  grid, skip missed ones). PC: an even grid stands in.
  Test: late_frames_stay_on_the_refreshes. Phone check pending: owner
  records another walk; re-run shift.py, expect no still frames mid-walk.

## 2026-10-26 (evening): whole GL picture on its side (SpellCraft hall, Dungeon Plunder map)

- Owner screenshots after the 18:25 install: game pictures turned a quarter turn
  and stretched, UIKit labels upright. Cause: the uncommitted "turned frames"
  present (meant for GL views that draw a tall buffer in an upright iOS 6
  window, Monster Life 640x1136) turned every old game's frame by the window
  turn, but SpellCraft/DP already draw a wide buffer.
- Fix: eagl.rs turn_for_picture: only a quarter turn, and only for a buffer
  taller than wide. Test only_tall_pictures_are_turned (deliberate break caught).

## 2026-10-26 (night): hall <-> garden flicker still there

- Owner: flicker switching between the main hall and the garden. Phone
  recording (~/.cache/ios-debug/spellcraft/flicker3/sc-flicker3.mp4, flash.py):
  during each 0.5 s cross-fade the game picture alternates frame by frame
  between the garden and the hall (strict one-on/one-off), with the new
  screen's UIKit labels steady on top. The installed build already had
  frame_slots (newest of close frames), so that was not enough.
- Cause: each screen's renderer (SCGreenhouseRenderer, SCTowerRenderer, ...)
  has its own renderbuffer (renderbufferStorage:fromDrawable: on its own
  layer), all on one EAGLContext. On an iPhone each buffer belongs to its
  own layer and the cross-fade blends the layers. Here every
  presentRenderbuffer: goes to the one window. At 120 Hz the two renderers'
  timers don't both fit in one refresh, so each got every other refresh and
  both reached the screen: frame_slots' half-refresh hold never saw them
  close together.
- Fix (all games): frame_slots.rs FrameSources: the buffer (context,
  bound renderbuffer) that began showing frames most recently has the
  window; one that showed nothing for 0.25 s takes it back when it starts
  again; an older buffer still drawing meanwhile is not shown (its layer
  would be fading out behind the new one). eagl.rs present() asks it first.
  The GL picture cuts to the new screen; UIKit still cross-fades.
  Test: the_newest_drawing_buffer_has_the_window (failed first with the
  same alternation as the phone).

## 2026-10-26 (night): big "Offline" bar at the bottom

- It is the game's news bar (AELiveBarView, run by AELiveBarManager):
  pictures and web pages from the game's server, which is gone, so it only
  says "Offline". Owner's choice: remove the whole bar, always.
- Fix (game_fixes.rs hide_news_bar): after -[AELiveBarView
  willMoveToWindow:] with a window, the bar is set hidden (its own code
  never shows or hides itself: Ghidra read of willMoveToWindow:,
  didMoveToWindow, setupLiveBarView, layoutLiveBarFrame). Hidden views get
  no touches; the manager still hears of the bar as before.
  Test: spellcrafts_news_bar_is_hidden_when_it_joins_a_window (failed first;
  deliberate break caught).
- Review (GPT-6.1 Sol, high, read-only) of the flicker fix, the news bar
  and the converter's Fix 6:
  - P1 a newer buffer that failed to take the window (eglMakeCurrent busy
    on another thread) kept it, hiding the shown one: it now lets go
    (FrameSources::let_go).
  - P2 our UIKit sends willMoveToWindow: through the host path, which ran
    no game fixes: dispatch_from_host now runs after_message too (test
    sends from the host as well; it failed first).
  - P2 an old screen still drawing after its layer left the window could
    take the window back when the new one paused: frames from a layer not
    in a window are never shown (frame_source, as on an iPhone); sources are
    keyed by layer when known (two buffers on one layer stay one source).
    Test only_buffers_on_screen_and_shown_keep_the_window (break caught).
  - P2 Fix 6 matched any ServiceConnection with that shape: now only classes
    under com/google/android/gms/ (test: a game's own class is left alone;
    break caught). Test dex made valid (p0 no longer v0). The patched Rule the
    Kingdom passes Android's own verifier (cmd package compile -m verify).

## 2026-10-26 (late): Dungeon Plunder walk still uneven after the vsync fix

- Owner video ~/.cache/ios-debug/dp-walk/brief/dp-walk3.mp4 (after the
  display-link fix), counted with dp-walk/tools/count.py (shift3.json):
  7.9 s of walking, 82 refreshes missed (gaps of ~17 ms, the next frame a
  double step), 22 still frames. The first video (shift.json, before the
  fix): 14.7 s, 13 missed, 110 still. So the fix cut the stills, but missed
  refreshes went from ~1 to ~10 a second. The game still presented 120 a
  second (its log), so frames were made but reached the screen too late.
  The video's own times jitter ±1.5 ms, so a double step often sits one
  frame from its 17 ms gap; "no dropped frames" in the first look missed
  that a 17 ms gap is one lost refresh at 120 Hz.
- Cause 1 (ours): frame_slots.rs held every main-thread frame half a
  refresh (4.2 ms) before eglSwapBuffers, to keep only the newest of two
  drawers (SpellCraft's cross-fade). With the display link now on the
  refreshes, a frame drawn in ~3-5 ms was swapped at ~7-9 ms after the
  refresh, right at Android's deadline, so about one in twelve missed it.
  An iPhone does not hold a lone drawer's frame: presentRenderbuffer: shows
  it at the next refresh. Fix: a lone drawer's frames go at once; frames
  wait only once main-thread frames have met within half a refresh in two
  refreshes running (two drawers), for 0.25 s after the last meeting. One
  late frame meeting the next on-time one does not count. The first
  frames of a cross-fade may show both screens once or twice; FrameSources
  (newest buffer has the window) still covers separate buffers.
  Tests: a_lone_drawers_frames_are_not_held (failed before: "frame 0 goes at
  once"); only_the_newest_of_close_frames_reaches_the_screen updated (only
  the newest from the second meeting on). Deliberate break (wait on any one
  meeting) caught by the first.
- Cause 2 (the game's own, not changed): the map moves one tile per
  CCMoveTo of 0.2 s (-[TileMapLayer centerTileMapOnTileCoord:tileMap:]
  0x1afec, 0x3e4ccccd); -[TileMapLayer update:] (0x1c16c) starts the next
  only when the map has no running action. cocos2d's -[CCActionInterval
  step:] (0xa183c) sets elapsed to 0 on a new action's first tick, so every
  tile starts with one frame that does not move: 24 moving frames + 1 still
  = 25 refreshes (0.208 s), the still frames seen every ~0.2 s. A real
  iPhone does the same (at 60 Hz one held frame in 13). Possible game fix
  if the owner wants it: a game_fixes entry that gives the first tick of a
  CCMoveTo started from TileMapLayer the frame's dt (or carries the time
  the last move ran over) instead of 0.
- Not checked on the PC: Android's frame deadline (needs the phone). Phone
  check: record a walk, run count.py; expect missed refreshes back near the
  first video's ~1 a second, and one still frame per tile (~4.8 a second).

## 2026-10-26 (late): Dungeon Plunder walk without the still frame per tile

- Owner: "Yes, smooth it out" (cause 2 above). Fix in game_fixes.rs, only
  when the game has TileMapLayer with centerTileMapOnTileCoord:tileMap:
  (checked once at start): the bridge follows -[CCNode runAction:] on a
  CCTMXTiledMap (runs it nested, not jumped into); after it, a CCMoveTo just
  started goes on from the last one (walk_on): its first tick, which only
  stands still, is used up at once (step: 0), then it is stepped by the time
  the last move ran past its end (cocos2d holds a move at its end and drops
  that time). Not after a move that was stopped part way. Everything goes
  through the game's own cocos2d methods (step:, elapsed, duration, isDone);
  the last move is kept (retained) so its time can be read after cocos2d
  lets it go. Speed: exactly 0.2 s a tile (before: 0.2 s + a frame).
- Why this hook: runAction: hands over the new move itself, right after
  cocos2d set it up; no game code or memory is patched, and other games
  (no TileMapLayer) are not followed.
- Test: dungeon_plunder_walks_on_without_a_still_frame_per_tile (a model of
  the game's step: at 120 Hz, frames up to 0.8 ms late). Failed before
  (a frame moving 0), passes; deliberate break (no carried time) caught
  (a frame moving 0.00002 of a tile). Phone check: record a walk, count.py:
  expect no still frames mid-walk.

## 2026-10-26 (late): tapping another iPhone game opened the last one

- Seen on the phone: after the game process was closed (by Android, or a
  game switch, which restarts the process), tapping Dungeon Plunder opened
  SpellCraft again. IosGameActivity is singleTask: Android brings the closed
  screen back with the request it first opened with, then hands the new one
  to onNewIntent. No game had started yet, so switchingGames ignored it and
  the screen started the old game.
- Fix: in onNewIntent, before a game starts, a request for another game
  becomes the screen's request and the screen is rebuilt (recreate), so its
  orientation rules come from the new game's Info.plist too.
- Check (phone): close the :ios process, tap Dungeon Plunder's Play: it must
  open Dungeon Plunder (before: SpellCraft).

## 2026-10-26 (night): garden drawer cards half hidden

- Owner: the cards in the bottom-right drawer (garden Inventory tab) are cut
  off at the bottom. Phone screenshot: cards start ~50 pt too low.
- Cause: -[SCCardHandView dynamicScrollView:contentViewForObject:atIndex:]
  makes a 160x200 SCCardView and gives it a 0.5 scale transform;
  -[AEDynamicScrollView layoutSubviews] then sets frame (i*w + border, 0,
  frame.size). Our view frame ignored the transform (stayed 0,0,160,200), so
  the card was placed 160x200 at y 0 and shown at half size around its
  center: y 50-150 in a 100 pt tall list.
- Fix (uikit.rs view_frame/view_set_frame, layers.rs place_in_frame): a
  transformed view's frame is its transformed box; setFrame: keeps the
  transform and picks bounds + center (Core Animation's way, same math as
  plain layers already used). A view's layer frame follows it.
  Test: a_transformed_views_frame_is_its_transformed_box (failed before).

## 2026-10-26 (night): Dueling / Social offline froze the hall

- Owner: tapped Dueling and Social (online only), then nothing could be
  tapped. Phone log: 14853x "objc_msgSend: guest object 0x24d1fe (isa
  0x69647561) is not known to the bridge (message initWithTitle:message:
  delegate:cancelButtonTitle:otherButtonTitles:)".
- -[SCBaseViewController actionQuad] (no network) makes a UIAlertView with
  otherButtonTitles:nil. 0x24d1fe is the selector string "audioController"
  that actionQuad keeps in its own stack frame two words further up.
- Cause: convert.rs Varargs::NilList read words after the last named
  argument even when that argument (the list's first object) was nil, so
  the caller's stack (the alert itself, then a selector) became buttons.
- Fix: a nil last named argument ends the list. Test: the nil-first case in
  variadic_arguments_go_to_stack_slots (failed before).

## 2026-10-26 (night): top bars never fill; real-money shop items

- Owner: the bars by the heart, the yellow circle and the level never move.
  PC trace (owner save copy, runs/bars2): SCProgressView gets setProgress:
  0.2 / 0.79 / 1 but -drawRect: is never called. Our UIKit never called
  drawRect: at all; setNeedsDisplay was a no-op. SCProgressView draws its
  fill there (clip to a UIBezierPath, CGContextDrawLinearGradient).
- Fix (uikit.rs display_tree/display_pass, graphics.rs draw_picture): a
  shown view marked for display (every view at first, setNeedsDisplay(InRect:),
  a resize with contentMode Redraw) whose class has its own drawRect: is
  drawn into its layer's contents (bounds at screen scale) before each
  picture tick (pictures.rs) and before renderInContext:. UIView gets a
  no-op drawRect: (for super calls). Test:
  views_with_their_own_draw_rect_draw_into_their_layer (failed first).
- Real-money items (owner chose: hide them): buying froze the game (store
  answered 'cannot connect', then 7055x "host object ... has unknown class
  (message objectForKey:)"; cause not found yet). game_fixes.rs: after
  -[SCStoreSectionView refreshContent], AEStoreKitItem items leave
  sectionContent, then the shop's scroll list refreshes. Test:
  real_money_items_leave_the_shop_list (failed first).
- Recipe List doesn't scroll: our UIScrollView has no drag scrolling at
  all. Sub-agent (DeepSeek, worktree sc-scroll, branch ios/sc-scroll) adds it.

## 2026-10-26: shop coin icon, drag scrolling
- Shop prices: NSNumberFormatter stringFromNumber: now goes through
  stringForObjectValue: (the game's currency formatter overrides that), so gold
  prices no longer show "$30.00".
- Coin icon: AEPriceLabel draws its coin in -drawTextInRect: and then calls super.
  Our UILabel now has drawRect: (calls drawTextInRect: on the bounds) and
  drawTextInRect: (draws the label's text into the current context). A label whose
  class replaces either one draws itself through the display pass, and its own text
  picture is left out. Plain labels still use the text picture. setText:/setFont:
  mark the label for redrawing. Test: labels_that_draw_their_own_text_draw_into_their_layer.
- Drag scrolling (Recipe List, clothing shop row): UIScrollView follows the finger
  once a drag passes 10 pt on an axis it can move. The content gets touchesCancelled,
  short taps still reach it, it keeps sliding after a flick, paging works, and the
  delegate hears scrollViewDidScroll: and the drag/slow-down calls. Code in
  controls.rs scroll_touch plus the touch.rs hook. Written by a sub-agent in
  worktree sc-scroll; its patch is ~/.cache/ios-debug/spellcraft/sc-scroll.patch.
- Full suite: 512 passed. Installed on the Pixel; owner to check the coin and scrolling.
- Not found: why the game froze after a failed real-money purchase ("host object
  ... has unknown class ... (message objectForKey:)"). It can't happen now because
  paid items are hidden.
- Follow-up (owner: "still can't scroll"): a phone trace showed the shop row
  (AEDynamicScrollView 320x100, content 1680x100) was found, but the finger was
  measured in window points. Old games turn their root view a quarter turn inside a
  portrait window, so a sideways swipe looked like an up-down one. The row only
  scrolls sideways, so it ignored the drag. Fixes in controls.rs:
  finger_motion measures each scroll view's drag in its superview's points, and
  setContentOffset: marks the scroll view for layout (AEDynamicScrollView adds the
  cards coming into view in -layoutSubviews). Tests:
  a_scroll_view_in_a_turned_view_follows_the_finger_on_screen (fails with window
  points: break-checked), scrolling_lays_the_scroll_view_out_again. 514 pass.
  Checked on the Pixel: the clothing shop row scrolls with new cards, the Recipe
  List scrolls and Hex of Chaos shows in full, the gold coin shows on gold prices.
- Hall taps: the Spell Library opens from the cauldron (1018,560 phone pixels),
  not the label. The Dormitory door is 532,400 and the Clothing Shop tab 2000,656.
- Smoothness (owner: "sideways scroll is not 120 Hz, a bit stuttery"). Phone
  measurements before: each picture list took ~14 ms to write and went out ~25 ms
  apart. Fixes in pictures.rs and controls.rs:
  - encode_lines writes numbers itself (push_number: millionths, trailing zeros
    cut) instead of format!: list write 14 ms -> ~3 ms. Test: list_numbers_are_short.
  - The tick gap is counted from when the last list was started, not finished.
  - No "coming frames" sampling while a scroll view is dragged or coasting
    (controls::scrolling()), so each list is one frame.
  - Coasting steps on the vsync grid (coast_delay) and asks for a draw at once
    (pictures::draw_soon).
  After: drag lists median 8.7 ms apart, coast median 9.7 ms (p90 18.7 ms before
  the draw_soon change). 515 tests pass; installed on the Pixel.
  Next if it still stutters: the phone reads the list on the UI thread with
  split/toFloatOrNull (IosPictureList.parse); move that off the UI thread or
  write a faster reader.

## 2026-10-27: monster heart hangs below the heart on the card

- Owner (screenshot): the red heart fill on the monster card sits on the
  paper under the heart frame instead of staying inside it.
- The heart is SCIconProgressView (CardHealthProgressIcon.uispec): base icon,
  fill icon, border icon. createSubviews gives the fill UIImageView
  contentMode 6 (bottom) and clipsToBounds YES; layoutSubviews sizes it to
  the health part at the base's bottom, so the full-size heart picture is
  shown bottom-aligned and cut by the short view.
- The fight card is a picture made with -renderInContext: (render.rs). Two
  faults there: content modes 3-12 were all drawn centred at own size, and a
  masking layer clipped only its sublayers, not its own contents. So the
  heart was centred in the short view and hung out of it. Same centring in
  compose.rs.
- Fix: one content_rect (render.rs, used by compose.rs too) matching the
  phone's ContentMode.place (0/3 stretch, 1 fit, 2 fill, 4-12 edges and
  corners); the mask clip is set before draw_own.
  Test: a_picture_of_a_view_keeps_a_clipped_bottom_picture_inside (failed
  before: rows 3-6 red instead of 4-5).

## 2026-10-27: matching Apple iOS 6.1 in every framework area (9-area fix round)

- Owner: fix SpellCraft by making every framework function behave like Apple's
  iOS 6.1 (freezes: stuck dungeon room, after-purchase freeze; dorm flash,
  sudden card reveal, bat coming back; general mismatches).
- Nine read-only agents compared our code with Apple's binaries, one area each
  (views, controls, coreanimation, graphics, objects, foundation, nibs, media,
  libsystem). Their reports, fixes and decisions are in
  ~/.cache/ios-debug/spellcraft/apple-compare/<area>/{REPORT,FIXES}.md, the
  open choices in apple-compare/DECISIONS.md, and the merge in
  ~/.cache/ios-debug/spellcraft/merge/MERGE-NOTES.md.
- Each area was fixed in its own worktree (fx-<area>), test first, then
  reviewed (DeepSeek v4.1 flash) and bug-hunted (Kimi / GPT-6.1 Terra), and
  the findings fixed. Then all nine were merged into one tree and copied here.
- Causes found for the reported problems (see apple-compare/ARGON-FREEZE.md,
  KIMI-TRANSITIONS.md):
  - Stuck room: a host send with fewer words than the game method's
    signature left a stale register; encounterCardSlidOff:finished:context:
    never ran. Plus the UIView didStop context going through id conversion.
  - Purchase freeze: game blocks kept by host collections were freed at the
    end of one message (media: one lasting wrapper per guest block).
  - Dorm flash / bat: the shared GL surface kept the last frame of a GL layer
    that had left the window (media: a GL layer's frame shows only while its
    layer is in the window, fades with it).
  - Card reveal: direct layer.transform changes skipped the open UIView
    animation and dropped 3D (coreanimation: full CATransform3D end to end).
- Merge: 666 tests pass, twice in a row. Phone app picture-list check passes.
- Not yet confirmed on the phone: all of the above.

## 2026-10-27 (round 2, transitions): real CATransitions, flips, curls, stacked dismiss

- The whole-screen `!\tfade` line is gone. A CATransition now takes only its
  layer from what it showed on the last list to what it shows now: the list
  carries `!\ttransition` (type, subtype, ms, begin, curve, progress range,
  the layer's place and size), the old pictures, `!\tto`, the new ones,
  `!\tend` (pictures.rs `pick_transitions`, `frames`). UIKit's view
  transitions are UIKit's own CATransitions (popAnimationState 0x48e22:
  oglFlip / pageCurl / pageUnCurl / fade, block duration and curve). The
  phone draws dissolve (old·(1-p) + new·p), moveIn/push/reveal, a 3D flip and
  our own page peel. Modal flip/dissolve/curl run on the window's layer.
- Pictures with perspective inside a group (clipped, masked, shadowed,
  rasterized) are drawn projectively in compose.rs.
- Dismissing a stack: the top screen slides away over the dismissing
  controller's view; the ones between go at once (Apple's documented rule).
- Details: ~/.cache/ios-debug/spellcraft/apple-compare/round2/transitions/FIXES.md.

## 2026-10-27 (round 3): button parts are real subviews; insertSubview off by one

- UIButton's image, background and title label are now its own subviews,
  placed by -layoutSubviews from the content/title/image rects, the way
  Apple's UIButton does (controls.rs, render.rs no longer draws them by
  hand). A title font, shrink or line-break setting a game makes now shows.
  Default title font kept at 15 pt (Apple's is 18 pt; owner's choice).
- insertSubview:aboveSubview: / belowSubview: with a view already in the
  list looked for the sibling before taking the view out, so it landed one
  place off. UIKit finds the sibling after the removal (uikit.rs
  `sibling_place`; test `a_subview_moved_above_or_below_a_sibling_lands_next_to_it`).
- Details: ~/.cache/ios-debug/spellcraft/apple-compare/round3/buttons/FIXES.md.
- 711 tests pass. Not yet confirmed on the phone.

## 2026-10-27: Play on the main screen did nothing (after round 2 real -dealloc)

- Symptom: main screen shows; Play does nothing. Log: the first failing host->game
  call `objc_msgSend: message handler to an object with unknown isa` (host sent a
  block), then the same error for every later call (the first fault is sticky).
- Cause: AERestRequest (AppyEngine networking) autoreleases itself and relies on
  its NSURLConnection to keep it: Apple's NSURLConnectionInternal -initWithInfo:
  retains the delegate (and CFRetains the connection); the block in
  -_withConnectionDisconnectFromConnection: releases both after the last callback
  (iOS 6.1 sim Foundation 0x1111ac, 0x111497). Ours never retained it, so with real
  -dealloc on, the request was freed before its "offline" failure; the game's
  invokeCompletionHandler block then read a freed object (`-handler`).
- Fix (fw/web.rs): connections hold their delegate until the failure callback has
  run (or -cancel, which now really cancels: no callback after it). Test
  `a_connection_holds_its_delegate_until_its_last_callback`.
- Also: long chains of game objects freeing each other (AEGameItemSlot /
  AETimeMonitor, ~40 deep) hit the translator's 64-call nesting limit
  ("-dealloc ... failed: guest calls nested too deeply", objects never freed).
  Apple's -dealloc just nests on the stack. Now a -dealloc due with fewer than
  16 nesting levels left waits for the safe point a level up (counted.rs ready
  hook, GuestSide::nested_calls_left). Test `a_long_chain_of_deallocs_frees_every_link`
  (failed at link 40 of 100 before).
- Checked: Play opens the hall on the Pixel (before the chain fix); 713 tests pass;
  emulator: SpellCraft menus clean (183 deallocs, no errors) until the known
  emulator GLES1 glIsEnabled crash; Infinity Blade 1 HD reaches gameplay.

## 2026-10-27 (evening): fade edges, fight strips, hall <-> greenhouse pop

- White line on the left during fades: the pictures layer is now drawn on a
  RenderNode of whole pixels (IosPicturesView `screen`), placed on the same
  whole pixels as GL (`IosPictureList.place`), as Apple draws each frame on a
  whole-pixel framebuffer. Owner confirmed fixed.
- Dark 18 px strips in fights: SpellCraft gets a 557-point-wide screen
  (uikit.rs `narrower`, by bundle id). Owner confirmed gone.
- Bright first column on the left: not in the pixels (screenshot is even up
  to the edge), stays with HW overlays off, and YouTube's black bars show the
  same line, so it is the OLED panel. Left as is.
- Greenhouse pop: a phone recording (screenrecord, frame by frame) showed the
  new screen in full for ~60 ms, then the old one back and the fade. Cause: a
  new GL view's frames showed as soon as the bridge sent the fade's list,
  while the phone was still decoding the fade's snapshot. Apple shows a
  CAEAGLLayer's frames from the commit that adds it. Fix in two steps:
  1. eagl `frames_show` follows the GL layers of the list up on the screen
     (pictures.rs `GlCommit`), not of the window;
  2. the phone says when a list is up (`nativePicturesUp`, sent from a frame
     commit callback after the frame that draws it), with a 250 ms fallback.
  A second recording after step 1 alone still showed one 8 ms frame; after
  the frame commit callback the owner saw none. Test:
  `gl_layers_count_from_when_the_phone_puts_their_list_up`.
