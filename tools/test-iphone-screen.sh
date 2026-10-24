#!/bin/bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/iphone-screen-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
JARS=$HOME/.gradle/caches/modules-2/files-2.1
jar() { find "$JARS/$1" -name "$2-$3.jar" | head -1; }
STDLIB=$(jar org.jetbrains.kotlin kotlin-stdlib 2.4.20)
COMPILER=$(jar org.jetbrains.kotlin kotlin-compiler-embeddable 2.4.20):$(jar org.jetbrains.kotlin kotlin-daemon-embeddable 2.4.20):$STDLIB:$(jar org.jetbrains.kotlin kotlin-script-runtime 2.4.20):$(jar org.jetbrains.kotlin kotlin-build-tools-api 2.4.20):$(find "$JARS/org.jetbrains.kotlin/kotlin-reflect" -name 'kotlin-reflect-*.jar' ! -name '*sources*' | head -1):$(find "$JARS/org.jetbrains.kotlinx/kotlinx-coroutines-core-jvm" -name '*.jar' ! -name '*sources*' | head -1):$(jar org.jetbrains annotations 23.0.0)

cat > "$WORK/Check.kt" <<'EOF'
import dev.braxten.gamebridge.IphoneScreen

fun info(sdk: String?, orientations: List<String>, initial: String? = null): Map<String, Any?> = buildMap {
    sdk?.let { put("DTSDKName", it) }
    put("UISupportedInterfaceOrientations", orientations)
    initial?.let { put("UIInterfaceOrientation", it) }
}

fun main() {
    val landscape = IphoneScreen.rules(info("iphoneos6.1", listOf("UIInterfaceOrientationLandscapeRight", "UIInterfaceOrientationLandscapeLeft")))
    check(landscape.legacy)
    check(landscape.orientation == IphoneScreen.Orientation.LANDSCAPE_RIGHT)
    check(landscape.request == IphoneScreen.Request.SENSOR_LANDSCAPE)
    check(landscape.surfaceReady(2400, 1080))
    check(!landscape.surfaceReady(1080, 2400))

    val portrait = IphoneScreen.rules(info("iphoneos7.1", listOf("UIInterfaceOrientationPortrait")))
    check(portrait.legacy)
    check(portrait.orientation == IphoneScreen.Orientation.PORTRAIT)
    check(portrait.request == IphoneScreen.Request.FIXED_PORTRAIT)
    check(portrait.surfaceReady(1080, 2400))
    check(!portrait.surfaceReady(2400, 1080))

    val bothPortraits = IphoneScreen.rules(info("iphoneos7.1", listOf(
        "UIInterfaceOrientationPortrait", "UIInterfaceOrientationPortraitUpsideDown",
    )))
    check(bothPortraits.request == IphoneScreen.Request.SENSOR_PORTRAIT)

    val landscapeRightOnly = IphoneScreen.rules(info("iphoneos6.1", listOf("UIInterfaceOrientationLandscapeRight")))
    check(landscapeRightOnly.request == IphoneScreen.Request.FIXED_LANDSCAPE)
    val landscapeLeftOnly = IphoneScreen.rules(info("iphoneos6.1", listOf("UIInterfaceOrientationLandscapeLeft")))
    check(landscapeLeftOnly.request == IphoneScreen.Request.FIXED_REVERSE_LANDSCAPE)

    val initialOnly = IphoneScreen.rules(mapOf(
        "DTSDKName" to "iphoneos5.0",
        "UIInterfaceOrientation" to "UIInterfaceOrientationPortrait",
    ))
    check(initialOnly.orientation == IphoneScreen.Orientation.PORTRAIT)
    check(initialOnly.request == IphoneScreen.Request.FIXED_PORTRAIT)

    val upsideDown = IphoneScreen.rules(info(null, listOf("UIInterfaceOrientationPortraitUpsideDown")).toMutableMap().apply {
        put("DTPlatformVersion", "7.1")
        put("MinimumOSVersion", "8.0")
    })
    check(upsideDown.legacy)
    check(upsideDown.orientation == IphoneScreen.Orientation.PORTRAIT_UPSIDE_DOWN)
    check(upsideDown.request == IphoneScreen.Request.FIXED_PORTRAIT_UPSIDE_DOWN)

    val newer = IphoneScreen.rules(info("iphoneos8.2", listOf("UIInterfaceOrientationPortrait")).toMutableMap().apply {
        put("MinimumOSVersion", "5.0")
    })
    check(!newer.legacy)
    check(newer.orientation == IphoneScreen.Orientation.LANDSCAPE_RIGHT)
    check(newer.request == IphoneScreen.Request.SENSOR_LANDSCAPE)

    val fallback = IphoneScreen.rules(emptyMap())
    check(!fallback.legacy)
    check(fallback.orientation == IphoneScreen.Orientation.LANDSCAPE_RIGHT)
    check(fallback.surfaceReady(2400, 1080))
}
EOF
java -cp "$COMPILER" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -cp "$STDLIB" -d "$WORK/out.jar" \
    "$ROOT/android/app/src/main/java/dev/braxten/gamebridge/IphoneScreen.kt" "$WORK/Check.kt" 2>&1 | grep -v '^warning' || true
java -cp "$WORK/out.jar:$STDLIB" CheckKt
echo "iPhone screen rules: pass"
