#!/bin/bash
# Checks ContentMode.kt (Game Bridge) on the PC: where a UIImageView's picture goes in its view for each
# of Apple's thirteen content modes, as the game screen (IosPicturesView) draws it.
# Needs the Kotlin compiler jars from the Gradle cache (kotlinc isn't installed).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/content-mode-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
JARS=$HOME/.gradle/caches/modules-2/files-2.1
jar() { find "$JARS/$1" -name "$2-$3.jar" | head -1; }
STDLIB=$(jar org.jetbrains.kotlin kotlin-stdlib 2.4.20)
COMPILER=$(jar org.jetbrains.kotlin kotlin-compiler-embeddable 2.4.20):$(jar org.jetbrains.kotlin kotlin-daemon-embeddable 2.4.20):$STDLIB:$(jar org.jetbrains.kotlin kotlin-script-runtime 2.4.20):$(jar org.jetbrains.kotlin kotlin-build-tools-api 2.4.20):$(find "$JARS/org.jetbrains.kotlin/kotlin-reflect" -name 'kotlin-reflect-*.jar' ! -name '*sources*' | head -1):$(find "$JARS/org.jetbrains.kotlinx/kotlinx-coroutines-core-jvm" -name '*.jar' ! -name '*sources*' | head -1):$(jar org.jetbrains annotations 23.0.0)

cat > "$WORK/Check.kt" <<'EOF'
import dev.braxten.gamebridge.ContentMode
fun main() {
    // A 40x20 picture in a 100x50 view.
    val want = mapOf(
        0 to listOf(0f, 0f, 100f, 50f),      // scale to fill
        1 to listOf(0f, 0f, 100f, 50f),      // aspect fit (same shape here)
        3 to listOf(0f, 0f, 100f, 50f),      // redraw: stretched like scale to fill
        4 to listOf(30f, 15f, 70f, 35f),     // center
        5 to listOf(30f, 0f, 70f, 20f),      // top
        6 to listOf(30f, 30f, 70f, 50f),     // bottom
        7 to listOf(0f, 15f, 40f, 35f),      // left
        8 to listOf(60f, 15f, 100f, 35f),    // right
        9 to listOf(0f, 0f, 40f, 20f),       // top left
        10 to listOf(60f, 0f, 100f, 20f),    // top right
        11 to listOf(0f, 30f, 40f, 50f),     // bottom left
        12 to listOf(60f, 30f, 100f, 50f),   // bottom right
    )
    for ((mode, rect) in want) {
        val got = ContentMode.place(mode, 100f, 50f, 40f, 20f).toList()
        check(got == rect) { "mode $mode: $got, Apple's is $rect" }
    }
    // Aspect fit and fill keep the shape: a 40x40 picture in 100x50.
    check(ContentMode.place(1, 100f, 50f, 40f, 40f).toList() == listOf(25f, 0f, 75f, 50f)) { "aspect fit" }
    check(ContentMode.place(2, 100f, 50f, 40f, 40f).toList() == listOf(0f, -25f, 100f, 75f)) { "aspect fill" }
}
EOF
java -cp "$COMPILER" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -cp "$STDLIB" -d "$WORK/out.jar" \
    "$ROOT/android/app/src/main/java/dev/braxten/gamebridge/ContentMode.kt" "$WORK/Check.kt" 2>&1 | grep -v '^warning' || true
java -cp "$WORK/out.jar:$STDLIB" CheckKt

# The game screen must place its pictures this way.
if ! grep -q 'ContentMode.place' "$ROOT/android/app/src/main/java/dev/braxten/gamebridge/IosPicturesView.kt"; then
    echo "IosPicturesView doesn't place pictures with ContentMode.place" >&2; exit 1
fi
echo "content modes: pass"
