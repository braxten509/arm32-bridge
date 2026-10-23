#!/bin/bash
# Checks ApplePng.kt (Game Bridge) on the PC: a made-up Apple "crushed" PNG (CgBI chunk, raw deflate,
# BGRA, premultiplied alpha, every PNG row filter) must come back as a normal PNG with the same pixels.
# Needs python3 with Pillow and the Kotlin compiler jars from the Gradle cache (kotlinc isn't installed).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/crushed-png-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
JARS=$HOME/.gradle/caches/modules-2/files-2.1
jar() { find "$JARS/$1" -name "$2-$3.jar" | head -1; }
STDLIB=$(jar org.jetbrains.kotlin kotlin-stdlib 2.4.20)
COMPILER=$(jar org.jetbrains.kotlin kotlin-compiler-embeddable 2.4.20):$(jar org.jetbrains.kotlin kotlin-daemon-embeddable 2.4.20):$STDLIB:$(jar org.jetbrains.kotlin kotlin-script-runtime 2.4.20):$(jar org.jetbrains.kotlin kotlin-build-tools-api 2.4.20):$(find "$JARS/org.jetbrains.kotlin/kotlin-reflect" -name 'kotlin-reflect-*.jar' ! -name '*sources*' | head -1):$(find "$JARS/org.jetbrains.kotlinx/kotlinx-coroutines-core-jvm" -name '*.jar' ! -name '*sources*' | head -1):$(jar org.jetbrains annotations 23.0.0)

python3 -I - "$WORK" <<'EOF'
import struct, sys, zlib
from PIL import Image
work = sys.argv[1]
width, height = 9, 6
pixels = []
for y in range(height):
    for x in range(width):
        alpha = [255, 0, 128, 200, 255, 60][y]
        pixels.append(((x * 29 + y * 7) % 256, (x * 13 + 100) % 256, (y * 41 + x) % 256, alpha))
Image.frombytes("RGBA", (width, height), bytes(b for p in pixels for b in p)).save(f"{work}/expected.png")
# Apple's form: premultiplied BGRA, each row with a different filter (row y uses filter y % 5).
rows = []
for y in range(height):
    row = bytearray()
    for x in range(width):
        r, g, b, a = pixels[y * width + x]
        row += bytes([b * a // 255, g * a // 255, r * a // 255, a])
    rows.append(row)
def paeth(a, b, c):
    p = a + b - c; pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else b if pb <= pc else c
raw = bytearray()
for y, row in enumerate(rows):
    kind = y % 5
    above = rows[y - 1] if y else bytearray(len(row))
    raw.append(kind)
    for i, value in enumerate(row):
        left = row[i - 4] if i >= 4 else 0
        upper_left = above[i - 4] if i >= 4 else 0
        guess = [0, left, above[i], (left + above[i]) // 2, paeth(left, above[i], upper_left)][kind]
        raw.append((value - guess) & 0xFF)
deflate = zlib.compressobj(9, zlib.DEFLATED, -15)
data = deflate.compress(bytes(raw)) + deflate.flush()
def chunk(kind, body):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
crushed = b"\x89PNG\r\n\x1a\n" + chunk(b"CgBI", bytes([0x50, 0x00, 0x20, 0x06]))
crushed += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
crushed += chunk(b"IDAT", data[:20]) + chunk(b"IDAT", data[20:]) + chunk(b"IEND", b"")
open(f"{work}/crushed.png", "wb").write(crushed)
EOF

cat > "$WORK/Check.kt" <<'EOF'
import java.io.File
fun main(args: Array<String>) {
    val fixed = dev.braxten.gamebridge.ApplePng.uncrush(File(args[0]).readBytes()) ?: error("not converted")
    File(args[1]).writeBytes(fixed)
}
EOF
java -cp "$COMPILER" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -cp "$STDLIB" -d "$WORK/out.jar" \
    "$ROOT/android/app/src/main/java/dev/braxten/gamebridge/ApplePng.kt" "$WORK/Check.kt" 2>&1 | grep -v '^warning' || true
java -cp "$WORK/out.jar:$STDLIB" CheckKt "$WORK/crushed.png" "$WORK/fixed.png"

python3 -I - "$WORK" <<'EOF'
import sys
from PIL import Image
work = sys.argv[1]
expected = list(Image.open(f"{work}/expected.png").convert("RGBA").getdata())
got = list(Image.open(f"{work}/fixed.png").convert("RGBA").getdata())
assert len(got) == len(expected), "size differs"
for i, (want, have) in enumerate(zip(expected, got)):
    assert have[3] == want[3], f"pixel {i}: alpha {have[3]} != {want[3]}"
    if want[3] >= 60:  # premultiplying loses a little color at low alpha
        assert all(abs(h - w) <= 255 // want[3] + 1 for h, w in zip(have[:3], want[:3])), f"pixel {i}: {have} != {want}"
print("crushed PNG: pass")
EOF
