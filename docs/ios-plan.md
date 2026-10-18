# Running 64-bit iPhone games (ios/)

Goal: play old iPhone games on the Pixel through Game Bridge. First targets
are Infinity Blade II and III (Unreal Engine 3), which ship 64-bit ARM code
next to their 32-bit code. That 64-bit code runs straight on the phone's own
processor; nothing is translated. The work is pretending to be an iPhone:
loading Apple's program format and standing in for the Apple system parts
the game calls.

Infinity Blade: Dungeons ships 32-bit code only and will later go through
the bridge's 32-bit translator instead.

## How it works

| Part | File | What it does |
|---|---|---|
| Program reader | `src/macho.rs` | Reads Apple's program format (Mach-O): the 64-bit part of a universal file, segments, sections, symbols, entry point, and the dyld fix-up lists (rebase, bind, lazy, weak). Newer "chained" fix-ups are not read yet (not needed for these games). |
| Loader | `src/loader.rs` | Places the program in memory, applies fix-ups, connects every import to a stand-in, sets memory access. |
| Stand-in table | `src/symbols.rs` | Apple name → address of our version. |
| Missing pieces | `src/trap.rs` | A small code stub per missing function that names it when called. Default: stop there. With `--keep-going`: return 0 and continue (used to list missing pieces, never for play). |
| Objective-C | `src/objc/` | Reads Apple's 64-bit class layout (class_t, class_ro_t, method lists, categories, protocols), makes selectors shared, moves game fields up when our parent class is bigger (as Apple's runtime does), runs `+load` and `+initialize`, `objc_msgSend` in assembly. Our Apple classes use the same layout, so game classes can be children of ours. |
| C library | `src/libsystem/` | Apple's libc, threads, Mach calls, dispatch, blocks. Apple-only differences handled here: extra (variadic) arguments are passed on the stack in 8-byte slots (`variadic!` trampolines + `varargs.rs`), different `struct stat`/`dirent` layouts, file flag and error numbers, Apple's static lock values and lock type numbers, a 192-byte `setjmp` buffer, character tables (`_DefaultRuneLocale`). |
| Apple classes | `src/fw/` | NSObject, NSString, NSArray/NSDictionary/NSNumber/NSEnumerator, NSData, NSBundle, NSUserDefaults (saved as a property list), NSNotificationCenter, NSProcessInfo, NSLog, property lists (XML and binary), UIApplication/UIApplicationMain. |
| Start-up | `src/run.rs` | Stand-ins ready → load → `+load` → C++ start-up functions → `main()`. |

The game sees real folders directly. Its writable home folder (Documents,
Library, tmp) is `$IOS_HOME`, or
`~/.local/share/gamebridge-ios/<bundle id>/` by default.

## Running on the PC

```bash
~/.cache/aarch64-cross/build.sh -p gamebridge-ios
qemu-aarch64 -L ~/.cache/aarch64-sysroot \
  ~/.cache/aarch64-cross/target/aarch64-unknown-linux-gnu/release/ios-run \
  run <unpacked ipa>/Payload/SwordGame.app [--keep-going]
```

`IOS_LIST_MISSING=1` prints every missing import at the end.
`IOS_DEVICE` sets the reported model (default `iPhone6,1`, iPhone 5s).
`IOS_SCREEN` sets the screen (default `568x320@2`, landscape).
`IOS_RUN_SECONDS` ends the run loop after that many seconds.

Or build and run in one go (log in `~/.cache/ios-run.log`):

```bash
IOS_RUN_SECONDS=15 ios/tools/run-pc.sh <App>.app [--keep-going]
```

Check that our fix-up lists match Apple's tool exactly:

```bash
ios/tools/check-fixups.sh <App>.app/<program>
```

## Step 1 result (2026-10-18)

- Fix-up lists match `llvm-objdump` exactly. IB2: 227,831 rebases,
  2,528 binds, 443 lazy, 7,281 weak. IB3: 257,034 / 4,638 / 523 / 7,616.
- IB2 in strict mode (nothing faked): the 140 game classes are read, both
  `+load` methods and all 106 C++ start-up functions run, then `main`,
  `UIApplicationMain`, and the game's own
  `-[IPhoneAppDelegate application:didFinishLaunchingWithOptions:]`.
  It stops inside it at the first missing piece, `+[UIDevice currentDevice]`.
- IB3 reaches the same point with no extra work.

Known shortcuts in step 1 (to fix in later steps):
- Objects are never freed (`retain`/`release` do nothing).
- Objective-C and C++ exceptions stop the run.
- The game's crash handlers (signals, Mach exception ports) are not installed.
- No run loop yet: `dispatch_async` to the main queue only queues work, and
  start-up ends after `didFinishLaunching`.
- `__error` gives the right Apple error number right after a failed call;
  a value the game writes there itself is not kept.

## Step 2 result (2026-10-18)

IB2 in strict mode: `didFinishLaunching` returns, `applicationDidBecomeActive:`
runs, the main run loop fires timers and queued work, and the engine thread
(`-[IPhoneAppDelegate MainUE3GameThread:]`) starts, logs
`This device is [iPhone6,1] ... scale is 2.000000` and starts its own
threads. The first stop is graphics: `glGenRenderbuffers` in
`-[EAGLView CreateFramebuffer:]` (step 3).

Added: UIKit basics (UIScreen, UIDevice, UIView/UIWindow/UIViewController,
CALayer/CAEAGLLayer, UIImage, UIColor), geometry functions, run loop,
NSTimer, CADisplayLink, NSThread, main-thread calls, locks and conditions,
NSSet, NSURL, NSDate, NSError, NSFileManager, NSLocale, regular expressions,
SQLite (built in), keychain kept in `<home>/Library/keychain.plist`, audio
session, video and sound stand-ins (nothing shown or heard yet), network
checks that always say "offline", web requests that fail as offline.

IB3 also reaches its run loop; its first stop is Game Center
(`+[GKLocalPlayer localPlayer]`), which belongs to step 5.

Still shortcuts after step 2:
- Videos are skipped (the game is told they finished). No sound.
- Network is always offline.
- Objects are still never freed.

Every import the program has that we don't (IB2): 202 functions, 38 data
symbols, 90 classes. Largest groups: OpenGL ES (`gl*`, passes almost
straight through), Core Graphics, CF/Foundation basics, AudioToolbox
(AUGraph), sockets/reachability, CommonCrypto (MD5/SHA1/HMAC/AES), keychain,
Game Center, StoreKit, iAd, Twitter, mail.

## Step 3 progress (2026-10-18)

On the PC (qemu with Mesa drawing in software, no window) IB2 now draws:
the intro scene (castle walls, the skip arrows) shows from about frame 20,
with no OpenGL errors. Frame 1 comes about 115 s into a qemu run; after
that about one frame a second (the phone will be far faster).

Added: OpenGL ES through the system library (Apple and extension names
mapped to ES 3), EAGLContext on EGL (shared contexts, handing a context
from the main thread to the engine thread), the iPhone 5s extension list
and limits, PVRTC textures decoded when the chip can't read them, BGRA
textures, shader fixes Mesa needs, frames saved as PNG pictures, touches
(`UITouch`/`UIEvent`, test taps), CommonCrypto hashes, JSON, archiving of
property lists, time zones, pasteboard, motion (reports "not available"),
and an Android screen (`IosGameActivity`) that runs the game in a window.

Bugs found on the way (details in `ios/DEBUGGING.md`):
- Reporting the PC's extension list made the game size textures for one
  format and fill them with another (memory corruption).
- KVO change keys had the wrong text, so the startup movie never started
  and the game waited for it forever.
- `pthread_mach_thread_np` answered for the calling thread instead of the
  asked one, so the engine thread thought it was the rendering thread and
  drew with no context.

Test switches: `IOS_FRAMES_DIR` (save frames), `IOS_FRAME_EVERY` (default
60), `IOS_FRAME_LIMIT` (stop after that many frames), `IOS_TAPS`
(`"seconds:x,y;seconds:x,y>x2,y2"` in points: taps, or swipes from x,y to
x2,y2), `IOS_STACKS_AT` (every thread's call chain
at that time). On the phone: `ios/tools/run-phone.sh <App>.app` (no window;
frames and log copied back).

A second-model review found 11 problems; all are fixed: texture uploads
now follow the game's row layout and pixel buffers, frame saving can't
overrun its memory, loading contexts get their own surface instead of the
window's, context hand-overs can't go stale, keyed archives use Apple's
real format (Apple-made archives read correctly), JSON is checked as
strictly as Apple does, unchanged fingers report "stationary", the game
screen follows window size changes, and the game screen is private to
Game Bridge (other apps can't start it with a folder of their choosing).

On the Android emulator (2026-10-18, x86_64 image running our ARM code
through Android's own ARM translation, host graphics card): IB2 runs in
Game Bridge's game screen at about 60 frames a second, plays its whole
opening scene, and is playable: tapping the move circle walks the knight
to the first fight, and holding the block button blocks. Frame 1 comes
about 9 s after start.

Emulator test recipe (AVD `ios_bridge_test`: android-36 x86_64, 16 GB
data, 6 GB memory, port 5562, `-gpu host`):
- No window: `ANDROID_SERIAL=emulator-5562 ios/tools/run-phone.sh <App>.app`
  (the game folder is copied to /sdcard/Download/ios once).
- In the window: `tools/build-app.sh`, `adb install -r`, `adb root`,
  `appops set dev.braxten.gamebridge MANAGE_EXTERNAL_STORAGE allow`, then
  `am start -n dev.braxten.gamebridge/.IosGameActivity --es app
  /sdcard/Download/ios/SwordGame.app` (root can open the private screen).
  Log: /data/data/dev.braxten.gamebridge/files/ios/<name>/ios-run.log.
  Test switches: `setprop debug.gamebridge.ios.env "KEY=value;..."`.

Fixed on the way: the window context must keep one surface (moving the
screen context from a private surface to the window lost its bound
renderbuffer on the emulator, so the game read a 0x0 screen and never
drew); small PVRTC mipmap levels; `-[NSFileManager
stringWithFileSystemRepresentation:length:]`.

On the Pixel 10 (2026-10-18, no window): IB2 starts, draws correctly with
no OpenGL errors, first frame at 7.6 s, then about 55 frames a second.

Starting a game on the phone: Game Bridge's search lists every unpacked
iPhone game (a folder ending in `.app` with 64-bit code, for example
`Download/ios/SwordGame.app`) as a card with its own name and icon and a
Play button. Press and hold the card for "Add to home screen" (the game's
own icon); after the first play the game also shows when you press and
hold Game Bridge's icon. The game screen runs as its own task, and opening
it again goes back to the running game.

## Step 4 result (2026-10-18): sound and videos

Sound (`src/fw/sound.rs`): the game's audio graph (3D and multichannel
mixers whose inputs ask the game for samples) and its music players
(`AVAudioPlayer`, MP3 and WAV) are mixed into one stereo stream played
through Android's low-latency output (AAudio). PC and no-window phone runs
pull the stream in real time instead; `IOS_SOUND_FILE` (or `IOS_SOUND=1`
for `run-phone.sh`) saves it as a WAV file.

- Music is decoded by one worker thread per player into a short queue
  (0.75 s). The sound thread never reads files, decodes, or waits for a
  player: a busy player is skipped for one turn.
- Stopping or disposing a graph waits until no game callback is running,
  so the game can free its sound data right after.
- 3D mixer: direction, distance (Apple's default inverse curve), gain,
  playback rate, on/off. Multichannel mixer: volume, pan, on/off. Reading
  a setting back gives what was set. A muted input keeps playing silently,
  so it keeps its place.
- Input formats this layer can't read (big-endian, signed 8-bit, unsigned
  16-bit, more than 8 channels) are refused when set instead of playing
  noise. Buffers the game hands back are checked against their real size.
- `currentTime`, looping and seeking follow what is actually heard.
- If the sound device goes away (headphones), it is reopened, retrying
  until it works.

Videos (`src/fw/media.rs`, `IosGameActivity`): `AVPlayer` videos play in
Android's own video player over the game, black around them. Touches
still reach the game, so its own skip handling runs. Pause holds the
picture and play goes on from there. The video goes away when the game
removes the view showing it, changes the player's item or layer, or the
video ends (then the game gets its "played to the end" message). Without
the phone app (PC and no-window tests) videos are skipped: the game is
told at once that they ended.

On the emulator the startup movie and the opening cine play with picture
and sound, and the game carries on when each ends. The cine's skip button
works when held (a quick tap doesn't skip, as in the game itself). A second-model review
found 15 problems; all are fixed. Unit tests: the mixer (pan, resampling,
on/off) and the music player (time, loops, seeking, end).

## Plan to play IB2 and IB3 on the phone (approved 2026-10-18)

Rule: Game Bridge will run more iPhone games later. Nothing is made for one
game only; anything that would be, the owner decides first.

1. Fix the IB2 crash when the second fight loads (Pixel, drawing thread,
   a call through a game object holding junk). Reproduce with the owner's
   save on the PC/emulator, log in `ios/DEBUGGING.md`. Done: the second
   fight loads 3 times in a row, then 30 minutes of play, no crash.
2. Sharper picture and speed: report a screen that matches the phone
   (2424x1080 on the Pixel 10, not 1136x640 with bars), maybe a newer
   device so the game picks better textures and edges; the owner picks
   from side-by-side pictures with frame rates. Done: full-screen sharp
   picture, steady 60+ fps, no repeated pauses over 50 ms in fights.
3. IB2 everyday play: leaving and coming back resumes the game; Android's
   edge swipes don't eat sword swipes; the missing Apple pieces (alerts,
   Game Center and store answering "not available", ...) filled in; saves
   survive updates and are in Game Bridge's backups; a crash shows a
   message and leaves a log readable without root. Done: the story plays
   through with no crash, resume works, an update keeps the save.
   Research (2026-10-18, details and scratch tools in
   ~/.cache/ios-debug/res-research/): UE3 draws at UIScreen bounds (read
   once in -[IPhoneViewController loadView]) x the per-device
   `MobileContentScaleFactor` (iPhone 5s: 2.0), read back from the
   renderbuffer; touches use the same factor. hw.machine picks the
   settings section (IB2 parses one digit: iPhone10+ = unsupported; IB3
   falls back to iPhone6/6 Plus). Coalesced_*.bin are AES-256-ECB with a
   key in the program. Options: report the window's shape (`1212x540@2`
   = native 2424x1080, ~3.6x the pixels; camera keeps its horizontal view,
   so ~21% less vertical view at 20:9), the same at a fraction with a
   smooth stretch, or 16:9 at full height (`960x540@2`, bars stay). Config
   also has bSmoothFrameRate=TRUE, MaxSmoothedFrameRate=30 (check).
   HUD (2026-10-18, scripts read with ~/.cache/ios-debug/hud-research/
   ubc.py; IB2 packages are uncompressed): `SwordPC.AdjustHUDRenderSize`
   lays the HUD out full-screen only when 1.7 < width/height < 1.8 (or
   exactly 1.5); otherwise it uses a 3:2 "iPad" box fitted to the width,
   which on wider screens is taller than the screen, so health bars,
   buttons and their tap areas go off the top and bottom. Measured on the
   Pixel: 960x540@2 full HUD, 1000x540@2 already clipped, 2.24:1 none.
   Recommended general rule (owner choosing): like iOS, an app with no launch storyboard keeps
   the shape of its largest launch image (IB2: Default-568h = 16:9), drawn
   at the phone's real height with side bars. All sizes ran ~57-59 fps.
4. IB3: PC, then emulator, then Pixel (Game Center first), then steps 1-3
   for it. Done: the same checks as IB2.
5. Second-model review of everything, all tests, docs, commit. Pushing is
   the owner's call.

Later: Game Bridge takes `.ipa` files; Infinity Blade: Dungeons through
the 32-bit translator.
