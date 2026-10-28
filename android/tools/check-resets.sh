#!/bin/bash
# Checks the phone app's game reset (GameResets.kt) on the PC's
# JVM with the Kotlin compiler from the Gradle cache (no kotlinc needed).
set -e
here="$(cd "$(dirname "$0")" && pwd)"
m=~/.gradle/caches/modules-2/files-2.1
jar() { find "$m/$1/$2/$3" -name "$2-$3.jar" | head -1; }
k=org.jetbrains.kotlin; v=2.4.20
stdlib=$(jar $k kotlin-stdlib $v)
cp="$(jar $k kotlin-compiler-embeddable $v):$(jar $k kotlin-daemon-embeddable $v):$stdlib:$(jar $k kotlin-script-runtime $v):$(jar $k kotlin-build-tools-api $v):$(find $m/$k/kotlin-reflect -name 'kotlin-reflect-*.jar' | sort | tail -1):$(jar org.jetbrains.kotlinx kotlinx-coroutines-core-jvm 1.8.0):$(jar org.jetbrains annotations 23.0.0)"
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
java -cp "$cp" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -cp "$stdlib" -d "$out/check.jar" \
  "$here/../app/src/main/java/dev/braxten/gamebridge/GameResets.kt" "$here/ResetCheck.kt" 2>&1 | grep -v '^warning' || true
java -cp "$out/check.jar:$stdlib" ResetCheckKt
