# C++ runtime for iPhone games (libstdc++.6)

Games built against Apple's GNU libstdc++ (`/usr/lib/libstdc++.6.dylib`,
old copy-on-write `std::string`, GCC 4.2.1 streams) get this library's
functions and data. `ios/build.rs` compiles it for the host (arm64 Linux and
Android) with Apple's arm64 layouts, joins it into one object and makes all
of its names private; `ios/src/cxx.rs` registers the Apple names listed in
`exports.txt` (made by `ios/tools/cxxrt-exports.sh`).

| Folder | From | What |
|---|---|---|
| `include/`, `src/` | Apple's libstdcxx-104.1 (github.com/apple-oss-distributions/libstdcxx), GCC 4.2.1 based | headers and library sources (strings, streams, locale, trees, lists) |
| `libsupc++/` | GCC 4.2.1 `libstdc++-v3/libsupc++` (+ `gcc/unwind-pe.h`) | type information, `__cxa_*` exceptions, personality routine, `new`/`delete` |
| `unwind/` | GCC 4.2.1 `gcc/unwind-dw2.c`, `unwind.inc` and headers; `gamebridge_unwind.*` is Game Bridge's | the unwinder: DWARF CFI, Apple compact unwind, register save/restore |
| `glue/` | Game Bridge | Objective-C exceptions on the C++ ones (as objc4) |
| `adapt/` | Game Bridge | `gamebridge_prelude.h`: Apple's 128-byte `mbstate_t`, `wint_t`, ctype classes |
| `test/` | Game Bridge | `throw_test`: arm64 iPhone program (C++ and Objective-C++) for the exception unit test (`ios/tools/build-cxx-test.sh`) |

Changes to the GCC/Apple files are marked `GAME BRIDGE`. Apple-specific
points kept or matched: `streamoff` is `long long`, `fpos` holds the 128-byte
`mbstate_t`, `char` is signed, Darwin `ctype_base` masks (answered by
libSystem's rune table), a `basic_filebuf` lock is 64 bytes, RTTI names with
bit 63 set compare by string (`__GXX_NONUNIQUE_RTTI_BIT`), file streams open
paths through libSystem's `fopen` path mapping. Left out of the exported
names: `long double` entry points (Apple's is 64-bit, the host's 128-bit),
functions taking a host `FILE*`, and variadic or `va_list` functions (Apple's
arm64 passes variadic arguments on the stack; only `std::__convert_from_v`,
which no known game imports).

The unwinder only walks frames of the game's Mach-O code and of this
runtime; an exception that would pass through other code (Game Bridge's
Rust, system libraries) with no handler before it ends as uncaught
(`std::terminate`, logged as STOP), so nothing those frames hold is skipped.

License: GPL version 2 or later with the GCC runtime library exception
(see each file's header and `COPYING`).
