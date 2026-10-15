# Game Bridge without root: plan (2026-10-15)

Goal: other people (no Magisk) can convert and install 32-bit games with the Game Bridge app.
Root stays supported: when `su` works, the app keeps today's path (no dialogs, original target SDK).

## Where root is used today (android/app/src/main/java/dev/braxten/gamebridge)
| Use | Today (root) | Without root |
|---|---|---|
| Gate on start (MainActivity.refresh) | "needs root" message | gone; root optional |
| File access to find APKs | `appops set MANAGE_EXTERNAL_STORAGE allow` | user turns on "All files access" (settings page opened by the app) |
| Install converted game | `pm install -r --bypass-low-target-sdk-block` via /data/local/tmp | PackageInstaller session (+ split pieces), REQUEST_INSTALL_PACKAGES, system confirm dialog |
| Copy game data (OBB) | `cp -r` + `chmod` as root | plain file copy: Android 11+ lets apps holding REQUEST_INSTALL_PACKAGES write other apps' Android/obb (verify on 17) |
| Remove game | `pm uninstall` | PackageInstaller.uninstall / ACTION_DELETE, system confirm dialog |
| Delete game file | File.delete, root rm fallback | File.delete (all files access) |

## Old games (target SDK below the phone's minimum)
Measured on the Pixel (Android 17, SDK 37): installs need targetSdkVersion >= 24
(`INSTALL_FAILED_DEPRECATED_SDK_VERSION: App package must target at least SDK version 24, but found 23`).
Pirates 23, Puzzle Craft 1 21, Puzzle Craft 2 23, Broken Age 22 are below; Card Wars 34, Angry Birds 26 fine.
- Converter: new `--min-target-sdk N`: raises android:targetSdkVersion in AndroidManifest.xml in place (same chunk
  walk as extract_native_libs; int value). If the attribute is missing, report it (no structural edit yet).
- The number is not hard-coded: the app installs as converted; when Android answers DEPRECATED_SDK_VERSION it reads
  N from Android's own message, re-converts with --min-target-sdk N and installs again. Root path never needs it.
- Behaviour change for games below 23: dangerous permissions are no longer granted at install. Test each such game
  without grants; if one needs them, add a step after install that opens the game's permission page (or have the
  bridge request them at start). 23 -> 24: only file:// sharing changes.
- tools/convert.sh gets the same flag for PC installs without --bypass.

## Signing key
Today the app signs with the PC's debug key copied into its assets (so PC and phone builds replace each other).
A public build must not ship that key. Public build (`tools/build-app.sh --public`): no key in assets; on first use
the app creates its own key in Android's key store (AndroidKeyStore, never leaves the phone) and signs with it.
Owner build: unchanged. Risk: uninstalling Game Bridge loses its key, so later updates of an already converted game
need that game uninstalled first (saves lost); the app says so in its Remove/About text.

## Not covered (later)
- Per-game settings that today need `adb setprop debug.arm32bridge.*` (e.g. Pirates' 60 fps cap).
- Publishing the app (GitHub release): ask the owner first.

## Tests
- Emulator arm32_bridge_test (Android 16, x86_64, app has no root): full flow with Pirates (Unicorn mode) and
  Puzzle Craft 1 (target 21 -> raised): find file, permissions setup, convert, install dialog, OBB copy, start,
  remove, delete file.
- Converter unit test for the target SDK edit (compiled manifest fixture).
- Phone (ask owner first): root path regression with one small game; non-root path by forcing it with a debug setting.

## Changes after review (codex gpt-6.1-sol, 2026-10-15)
- OBB copy needs the user's "Install unknown apps" switch on (that app-op is what opens other apps' Android/obb to
  the installer), not just the declared permission. After copying, check the game can read it (first start).
- Installer: one PackageInstaller session (MODE_FULL_INSTALL) per game with all its pieces, unique names, streams
  closed before commit; a non-exported receiver with a MUTABLE PendingIntent (immutable is refused when the app
  targets 35+); STATUS_PENDING_USER_ACTION -> start EXTRA_INTENT, then wait for the final result. Results are
  structured (success / cancelled / other key / blocked / failed). Conversion files kept until the session ends.
- Target SDK 23->24 changes more than file:// sharing (private-library access, certificate trust, private file
  modes). So: raise only when Android refuses, only to the number it asks for, and test every affected game.
  Reading N from the status message is best effort: accept 1..(phone's SDK), retry once, else plain failure.
- Games below 23 lose install-time permissions. Game Bridge can't grant them; first answer: test the games, and if
  one needs them, the bridge (running inside the game) asks at start, or the app opens the game's permission page.
- Signing: AndroidKeyStore RSA-2048, PURPOSE_SIGN, PKCS1 padding, digests SHA-256 + SHA-1 (v1 for games whose
  minimum is below 18), no user authentication, long validity. Public builds also need a real key for Game Bridge
  itself (build.gradle signs release with the debug key): owner decision when publishing.
- Tests: the app is ARM64-only, so the x86_64 emulator can't run it as is. Test on the Pixel (Android 17) with a
  debug setting that forces the no-root path (owner's OK needed for phone use); converter checks on the PC.
