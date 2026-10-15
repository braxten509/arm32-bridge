# Game Bridge without root: plan (2026-10-15)

Goal: other people (no Magisk) can convert and install 32-bit games with the Game Bridge app.
Update 2026-10-15: after the phone test passed, the root path was removed from the app (owner's choice); every phone
uses Android's installer. Sections below describe the plan as it was, root path included.

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

## Built (2026-10-15)
- `Installer` (Root.kt) with `RootInstaller` (today's path) and `SessionInstaller` (Android's installer, one session
  per game, confirm screen, uninstall through PackageInstaller, plain-file OBB copy). `Installers.current` picks root
  when Magisk allows it; `adb shell setprop debug.gamebridge.noroot 1` forces the no-root path (restart the app).
- Converter.kt: when Android refuses an old game, the number it asks for is saved and the game converted again with
  `--min-target-sdk`, once; later games get it up front. As-is bundles can't be raised (their own signature).
- Signing.kt: the assets debug key when present, else an AndroidKeyStore key (alias `gamebridge-games`).
- Setup screen (sketch A): "All files access" and "Install unknown apps", checked again on return from Settings.
- `tools/build-app.sh --public` builds without the PC key. tools/convert.sh keeps `adb install --bypass-low-target-sdk-block`
  (adb's shell may use that flag without root), so it needs no change.
- Code review (codex gpt-6.1-sol): fixed API 31/32 intent getter; app restarts itself once after "Install unknown
  apps" is turned on (Android/obb opens only at process start); a confirmation Android wouldn't show in the
  background is shown when the app comes back (Confirmation); a confirmation left unanswered for 30 minutes
  abandons the session. Silent updates only apply to games targeting 35+, so old games always ask.

## Root path removed (2026-10-15)
Phone test (forced no-root): Puzzle Craft 1 refused at target 21, converted again at 24, installed with one
confirmation, opened without a crash. The owner chose to drop the root path: Root.kt, RootInstaller, Installers and
debug.gamebridge.noroot are gone; `Installer` (was SessionInstaller) is the only installer.
Broken Age (target 22 -> 24, `tools/convert.sh --min-target-sdk 24`, installed as an update): loaded and played about
a minute on the Pixel; no crash, no permission errors; Android later removed it from the background as usual.

## Release signing (2026-10-15)
`tools/release-key.sh` made Game Bridge's release key once: ~/dev/game-bridge-key/gamebridge-release.p12 (RSA 4096,
10000 days, alias gamebridge, SHA-256 22:9F:A8:62:D0:B3:B7:C1:0C:4E:19:DC:7F:32:F8:35:7E:2E:DE:9A:2E:1F:EE:C2:48:6E:2E:77:20:C3:1D:49),
random password in KDE Wallet (folder GameBridge, entry release-key). Both are in the daily Kopia backup.
`tools/build-app.sh --public` signs with it and writes android/GameBridge-<version>.apk. A phone with an owner build
(debug key) must uninstall Game Bridge before installing a public build.
