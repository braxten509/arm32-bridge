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

## Step 2 to-do: what IB2 still needs

From a `--keep-going` run, the pieces reached during `didFinishLaunching`:

- UIKit: UIDevice, UIScreen (`mainScreen`, `screens`), UIWindow, UIView,
  UIViewController (`view`), UIImage/UIImageView, UIApplication settings
  (`setApplicationSupportsShakeToEdit:`, `setApplicationIconBadgeNumber:`).
- Foundation: NSClassFromString, NSMutableSet, NSThread
  (`detachNewThreadSelector:toTarget:withObject:`), NSTimer, run loop.
- Sound session: AudioSessionInitialize/GetProperty/AddPropertyListener,
  AVAudioSession.
- Video: AVURLAsset, AVPlayerItem, AVPlayer (the intro movie).
- Keychain: SecItemCopyMatching (save data may live there → store in a file).

Every import the program has that we don't (IB2): 202 functions, 38 data
symbols, 90 classes. Largest groups: OpenGL ES (`gl*`, passes almost
straight through), Core Graphics, CF/Foundation basics, AudioToolbox
(AUGraph), sockets/reachability, CommonCrypto (MD5/SHA1/HMAC/AES), keychain,
Game Center, StoreKit, iAd, Twitter, mail.

## Later steps

3. Screen, drawing and touches on Android (EAGLContext/CAEAGLLayer → EGL
   surface), then the first picture inside an Android app.
4. Sound (AUGraph, AVAudioPlayer) and the intro videos.
5. Game Center, store, ads, iCloud answer "not available"; keychain → file.
6. Check the games' texture formats on the Pixel and decode if needed.
7. Game Bridge takes `.ipa` files.
8. Infinity Blade III, then Dungeons through the 32-bit translator.
