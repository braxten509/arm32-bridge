# 64-bit games in Game Bridge: list them and lift their frame caps

Owner's request (2026-10-17): Game Bridge also shows downloaded 64-bit games and
uncaps their frame rate like it does for 32-bit ones. Owner's answers:

- Only downloaded game files (APKs and bundles on the phone). No Play Store installs.
- Full lifting: the general steps plus Unity's and Unreal's own fps settings.
- No phone tests (no opening games or tapping on the Pixel): the emulator
  `arm32_bridge_test` only. It runs arm64 code through Google's ndk_translation.
- Test game: Taverns & Quests 3.0.10 (~/dev/arm32-bridge-games/taverns/TavernsQuests.xapk:
  base + config.arm64_v8a + UnityDataAssetPack; Unity 2022.3.62f3, IL2CPP, Swappy).

## Why 64-bit games need something new

32-bit games run through the bridge, which sees every call the game makes:
it asks for 120 Hz (Surface.setFrameRate), shortens frame-limiter sleeps on the
thread that calls eglSwapBuffers (threads.rs), and the converter removes
Thread.sleep in onDrawFrame (dex.rs fix 3). A 64-bit game runs directly on the
CPU, so a helper library has to be put inside it and loaded before the game's
own libraries.

## Parts

### 1. Helper library `libgamebridge_pacer.so` (new crate `pacer/`, Rust, arm64)

Hooks calls from the game's own libraries (libraries under the app's install
folder or its data folder, never system libraries, never itself) by rewriting
their import tables (GOT entries of JUMP_SLOT, GLOB_DAT and Android's packed
relocations), found through dl_iterate_phdr. RELRO pages are made writable for
the write and read-only again.

Why not ByteDance's bytehook: tested 2026-10-17 on the emulator, bytehook 1.0.10
fails to start there (status 3: it reads Android linker internals that the ARM
translator doesn't have), and bytehook 1.1+ needs shadowhook (inline hooks in the
linker). A 60-line test of plain import-table patching worked on the emulator
for a library already loaded and one loaded later (scratch fixture gotcheck).

Libraries loaded later: `dlopen` and `android_dlopen_ext` are hooked in the
app's libraries and in libnativeloader.so (System.loadLibrary on the phone);
after each load, new libraries are patched. As a safety net (and the only way on
the emulator, where System.loadLibrary goes through the translator), a thread
checks dl_iterate_phdr's load counter every 1 ms for 30 s, then every 20 ms, and
logs any library it had to catch that way.

General steps (any engine):
- eglCreateWindowSurface / vkCreateAndroidSurfaceKHR: call
  ANativeWindow_setFrameRate(window, 120) on the game's window.
- ANativeWindow_setFrameRate / ANativeWindow_setFrameRateWithChangeStrategy
  with a rate below 120: raised to 120.
- eglSwapInterval(n > 1): becomes 1.
- eglSwapBuffers(WithDamageKHR) / vkQueuePresentKHR: mark the frame thread and
  the frame time.
- nanosleep / usleep / clock_nanosleep up to 100 ms on the frame thread: the
  same rule as the 32-bit limiter (threads.rs): the sleep ends one refresh after
  the last frame, minus the work that follows it. Ported, not shared, because
  the bridge's version reads guest state.
- Per-game cap: the same `debug.arm32bridge.unlock.<package>` property
  (0 = keep the game's limits, N = cap at N fps, never below 60 per owner rules).

Unity (IL2CPP and Mono): Unity's engine gets il2cpp_*/mono_* functions with
dlsym. The helper hooks dlsym in the app's libraries:
- `il2cpp_add_internal_call` / `mono_add_internal_call`: remembers the function
  registered for `UnityEngine.Application::get_targetFrameRate` and wraps
  `set_targetFrameRate` when the game registers it (game's value -1 or below
  the target becomes the target). With IL2CPP, if the registration was missed,
  `il2cpp_resolve_icall` finds the getter.
- The getter in Unity 2022 is `[b X] -> adrp xN, P; ldr w0, [xN, #o]; ret`, and
  Unity's own setter is the function right after it:
  `adrp xN, P; str w0, [xN, #o]; b <apply>`. Taverns & Quests strips the C#
  setter, so the helper finds the engine setter by this pattern: every
  instruction must lie in the same executable segment of libunity, both must
  address the same data word, and the setter must end in a branch or return.
  Anything else: log it and change nothing.
- `il2cpp_runtime_invoke` / `mono_runtime_invoke`: Unity calls these on its main
  thread after start-up. The first call after the getter is known calls the
  setter (keeping its apply step) and logs `targetFrameRate <old> -> <target>`
  and the thread name. After that, at most once a second, if the getter reads a
  different value that the game didn't ask for through the wrapped setter, it is
  set again and logged (catches engine resets).

Unreal: deferred. Its cap is the game thread sleeping in
UEngine::UpdateTimeAndHandleMaxTickRate plus mobile frame pacing
(r.SetFramePace, Swappy, device profiles); config paths differ between versions.
Without a packaged shipping Unreal test game this can't be checked, so only the
general steps apply to Unreal games for now. Owner to be told; an Unreal test game
would unblock it.

All actions log under tag `gamebridge` (`[pacer] ...`).

### 2. Loader: a content provider

A tiny Java ContentProvider (`dev.braxten.gamebridge.Pacer`, loads
the helper in a static initializer, so on class load) compiled with javac + d8 by
tools/build-pacer-dex.sh into converter/assets/pacer.dex (checked in, like the
generated GLES table). Android creates content providers before
Application.onCreate and before any activity, so it loads before the game's
libraries (a game loading libraries in attachBaseContext is the known gap).

The converter adds it as the next free classesN.dex (consecutive name; dex
version 035 so every Android it may run on reads it) and adds to <application>:
`<provider android:name="dev.braxten.gamebridge.Pacer" android:authorities="<package>.gamebridge.pacer" android:exported="false" android:initOrder="2147483647"/>`
(highest initOrder: created before the game's own providers). Known gap:
Android creates the Application object (and runs attachBaseContext) before any
provider; a game loading native code there loads it first. The helper logs every
app library already loaded when it starts, so such games show up.
manifest.rs gets a structural edit that never renumbers existing strings: new
strings are appended at the end of the pool, the resource map is extended to
cover new attribute-name strings (0 for the strings in between), a start/end
element pair is inserted after <application>'s start, sizes fixed. Tests: UTF-8
and UTF-16 pools, styled pools, no resource map, adding twice, real manifests;
aapt2 dump xmltree diff (only the provider lines may differ).

### 3. Converter 64-bit mode

`apk-convert` takes `--pacer <libgamebridge_pacer.so>`. For a game with arm64
code (APK or bundle): add the helper to lib/arm64-v8a (stored, page-aligned
like the bridge today), add the dex + provider, apply dex fixes (fix 3 matters
here), keep everything else. For a bundle, the helper goes in the arm64 config
piece, dex + manifest in the base. `--inspect` keeps reporting bits 64.
32-bit conversion is unchanged.

### 4. Game Bridge app

- Games.scan lists APKs with arm64 code too (Action.Convert, same card and
  button; no look change). 64-bit bundles change from "Install as-is" to Convert.
  Bundles without any native code keep Install.
- Converter.kt passes `--pacer` (the app ships the helper in its own lib folder).
- isConverted also counts libgamebridge_pacer.so in the game's lib folder.
- tools/build-app.sh builds the helper.

## Pass checks (each must pass; a check that fails 3 fixes in a row = stop and ask)

1. `cargo test` in converter: all old tests pass, new manifest/dex/ini tests pass.
2. aapt2 dump xmltree of the converted Taverns & Quests base shows the provider;
   `apk-convert --inspect` of the output still says bits 64.
3. Emulator: converted Taverns & Quests installs and reaches its main menu with
   no crash (logcat, screencap), and logcat shows the helper loaded first,
   libunity/libil2cpp hooked, `targetFrameRate -1 -> 120` (on UnityMain) and the
   window rate request.
4. Emulator fps (SurfaceFlinger timestats, same session): the original measured
   23.2 fps at the menu, below its 30 cap, because the emulator (ARM translation +
   software drawing, ~6 PC cores) is the limit. So fps can't prove the uncap
   here. Instead: the getter reads the target after the set and still reads it
   60 s later (logged), and the converted game's fps is not lower than the
   original's. Reported plainly as partial: real fps needs the phone.
5. Game Bridge on the emulator lists a 64-bit APK and the T&Q bundle with
   Convert, and converting T&Q from the app works.
6. No change for 32-bit games: converter tests for 32-bit pass, and Puzzle Craft 2
   converted by the new converter is byte-identical to the old converter's output.

Files allowed to change: pacer/ (new), converter/src/*,
converter/assets/pacer.dex (new), tools/build-pacer-dex.sh (new),
tools/build-app.sh, tools/convert.sh, build-phone.sh, Cargo.toml (workspace),
android/app/src/main/java/dev/braxten/gamebridge/{Games,Converter}.kt,
docs/64bit-uncap-plan.md, DEBUGGING.md, README.md (games list note).
