> [!NOTE]
> **This project was made with AI.** The code, the docs and this README were written by an AI coding assistant
> (Anthropic's Claude), directed and tested on a real phone by Braxten Chenay. Expect rough edges.

# Game Bridge (arm32-bridge)

Play old 32-bit Android games on new phones that can only run 64-bit apps.

Newer phones like the Pixel 10 have processors without a 32-bit mode, so many older games (made before 2019 or
so) won't install at all ("App not compatible"). Game Bridge converts such a game on the phone itself: it swaps
the game's 32-bit code for a bridge that translates it to 64-bit while the game runs, then installs the result.
The game keeps its own look, sound and save files.

## Download

Get `GameBridge-<version>.apk` from the [Releases page](https://github.com/braxten509/arm32-bridge/releases)
and open it on your phone. No root needed.

**Needs:** a 64-bit ARM Android phone with Android 12 or newer. Only tested on a Pixel 10 with Android 17
(GrapheneOS). If your phone still runs 32-bit apps, you don't need this; install the game normally.

## How to use it

1. Download the game you own as an APK file (or a bundle: `.xapk`, `.apkm`, `.apks`) onto the phone.
2. Open Game Bridge. The first time, it asks for two things in Android's settings:
   - **All files access**, so it can find game files in Downloads and your other folders.
   - **Install unknown apps**, so it can install the converted game. (Game Bridge restarts itself once after
     this.)
3. Your 32-bit games show up as cards. Tap **Convert**, wait, then tap **Install** on Android's screen.
4. Tap **Open** and play. Press and hold a card for more (Convert again, Remove, Delete file).

## Games that have worked so far

> [!WARNING]
> **Most games have never been tried.** Only the games below have been tested, all on one phone (Pixel 10,
> Android 17). Any other game may not start, may crash partway through, may have missing sound or graphics, or
> may run much slower than it would on a phone with 32-bit support. Even the games below can behave differently
> on other phones or other versions of the game. Treat everything else as an experiment.

| Game | Version tested | How it runs |
|---|---|---|
| Card Wars | 1.12.9 | plays |
| Puzzle Craft | 1.1.3 | plays |
| Puzzle Craft 2 | 1.5.2 | plays |
| Assassin's Creed Pirates | 2.9.1 | plays, with some stutter |
| Broken Age | 2.2.2 | plays |
| Angry Birds Classic | 8.0.3 | plays |

When a game uses phone functions the bridge doesn't know yet, the converter lists them; the game may then fail
when it reaches them.

## Good to know

- **No games are included.** Bring your own copies of games you own. This project is not connected to any game
  maker or store.
- **Converted games are signed by Game Bridge, not by their maker.** If the original game is still installed
  (for example from the Play Store), uninstall it first; that deletes its saved progress.
- **Old games get a small label change.** Android 17 refuses games made for Android 6 or older, so Game Bridge
  marks them as made for Android 7. Most games don't notice; a very old game could act differently.
- **Each phone has its own key.** Game Bridge makes a signing key for games on the phone and keeps it there. If
  you uninstall Game Bridge, later updates of a converted game need that game uninstalled first.
- Online features, store purchases and license checks inside games may not work.
- Speed depends on the game. Most run at full speed; heavy 3D games can stutter during loading.

## For developers

The bridge loads the game's 32-bit ARM libraries into a 64-bit process, translates their code to ARM64 as it runs
(`src/dynarec/`, see [docs/translator.md](docs/translator.md)) with [Unicorn](https://www.unicorn-engine.org/)
as a fallback, and answers the game's calls to the phone (libc, OpenGL ES, OpenSL ES, JNI and more) with 64-bit
code. `converter/` (apk-convert) rewrites an APK or bundle to load the bridge.

```sh
./build-phone.sh                      # bridge for the phone (arm64 Android), needs NDK 30
tools/convert.sh game.apk             # convert on the PC and install over adb
tools/build-app.sh [--install]        # build the Game Bridge app (owner build, PC debug key)
tools/build-app.sh --public           # release build, signed with the release key (tools/release-key.sh)
cargo build --release                 # PC build, for quick tests with tests/guest
```

More notes: [docs/no-root-plan.md](docs/no-root-plan.md) (how the app installs without root).

## License

Copyright (C) 2026 Braxten Chenay

This program is free software; you can redistribute it and/or modify it under the terms of the GNU General
Public License as published by the Free Software Foundation; either version 2 of the License, or (at your
option) any later version. See [LICENSE](LICENSE).

The bridge and the converter are built with [Unicorn](https://www.unicorn-engine.org/) (GPL version 2, patched
copy in `vendor/unicorn-engine-sys`, changes listed in its ARM32-BRIDGE.md), so those programs as built are
shared under GPL version 2.
