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
