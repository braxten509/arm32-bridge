// A second test library that selftest.cpp opens at run time (dlopen), the
// way a game's starter library opens the real game.
#include <jni.h>

static int started = 0;
__attribute__((constructor)) static void start(void) { started = 41; }

// 42 once the start-up code above has run.
int second_value(void) { return started + 1; }

// Its own JNI_OnLoad: dlsym on this library's handle must find this one, not selftest's.
JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void)vm;
    (void)reserved;
    return 0x7777;
}
