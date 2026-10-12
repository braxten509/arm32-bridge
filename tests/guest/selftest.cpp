// Self-test for the bridge: each check exercises one part (loader,
// calling convention, libc stand-ins, guest callbacks, setjmp, C++
// exceptions, the pretend Java side). run_all() returns the failure count.
#include <android/log.h>
#include <jni.h>
#include <math.h>
#include <pthread.h>
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdexcept>
#include <string>
#include <vector>

#define TAG "selftest"
static int failures = 0;
static void check(bool ok, const char *what) {
    __android_log_print(ok ? ANDROID_LOG_INFO : ANDROID_LOG_ERROR, TAG, "%s %s", ok ? "ok  " : "FAIL", what);
    if (!ok) failures++;
}

// A C++ static constructor: runs from init_array during loading.
static std::string built_at_load = std::string("made") + " at load";
static int constructor_ran = 0;
__attribute__((constructor)) static void mark_constructor() { constructor_ran = 42; }

static int compare_ints(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }
static int once_count = 0;
static void once_function() { once_count++; }
static jmp_buf jump;
static void jump_back() { longjmp(jump, 7); }
static double take_mixed(int a, double b, int c, long long d, float e) { return a + b + c + (double)d + e; }

extern "C" int run_all() {
    check(constructor_ran == 42, "constructor ran at load");
    check(built_at_load == "made at load", "C++ static object built at load");

    char *text = (char *)malloc(64);
    strcpy(text, "hello");
    strcat(text, " world");
    check(strlen(text) == 11 && strcmp(text, "hello world") == 0, "malloc + string functions");
    text = (char *)realloc(text, 4096);
    check(strncmp(text, "hello world", 11) == 0, "realloc keeps contents");
    free(text);

    char buffer[128];
    snprintf(buffer, sizeof buffer, "%d|%5s|%.2f|%lld|%x|%c|%u", -12, "ab", 3.14159, 1234567890123LL, 255, 'Z', 4000000000u);
    check(strcmp(buffer, "-12|   ab|3.14|1234567890123|ff|Z|4000000000") == 0, "snprintf with 32-bit argument sizes");

    check(fabs(sin(1.0) - 0.8414709848) < 1e-9 && fabs(pow(2.0, 10.0) - 1024.0) < 1e-12, "double math (softfp pairs)");
    check(fabsf(sqrtf(2.0f) - 1.4142135f) < 1e-6f && fabsf(atan2f(1.0f, 1.0f) - 0.7853982f) < 1e-6f, "float math");
    volatile double (*mixed)(int, double, int, long long, float) = (volatile double (*)(int, double, int, long long, float))take_mixed;
    check(mixed(1, 2.5, 3, 10000000000LL, 0.5f) == 10000000007.0, "mixed int/double/long long/float arguments");

    int numbers[] = {5, 3, 9, 1, 7, 2, 8};
    qsort(numbers, 7, sizeof(int), compare_ints);
    check(numbers[0] == 1 && numbers[3] == 5 && numbers[6] == 9, "qsort calling back into 32-bit code");

    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, once_function);
    pthread_once(&once, once_function);
    check(once_count == 1, "pthread_once calls back exactly once");

    pthread_key_t key;
    pthread_key_create(&key, nullptr);
    pthread_setspecific(key, (void *)0x1234);
    check(pthread_getspecific(key) == (void *)0x1234, "thread-specific data");

    int jumped = setjmp(jump);
    if (jumped == 0) jump_back();
    check(jumped == 7, "setjmp / longjmp");

    bool caught = false;
    try {
        std::vector<int> values(3);
        (void)values.at(10);
    } catch (const std::out_of_range &) {
        caught = true;
    }
    check(caught, "C++ exception thrown and caught (unwinder)");

    FILE *file = fopen("/proc/self/status", "r");
    char line[256] = {0};
    check(file && fgets(line, sizeof line, file) && strncmp(line, "Name:", 5) == 0, "fopen + fgets");
    if (file) fclose(file);

    errno = 0;
    check(strtol("99999999999", nullptr, 10) == 2147483647L && errno == ERANGE, "strtol clamps to 32-bit long + errno");

    __android_log_print(ANDROID_LOG_INFO, TAG, "%d failures", failures);
    return failures;
}

static JavaVM *saved_vm;
extern "C" JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *) {
    saved_vm = vm;
    JNIEnv *env = nullptr;
    if (vm->GetEnv((void **)&env, JNI_VERSION_1_4) != JNI_OK) return -1;
    jclass helper = env->FindClass("org/cocos2dx/lib/Cocos2dxHelper");
    jmethodID method = env->GetStaticMethodID(helper, "getCurrentLanguage", "()Ljava/lang/String;");
    jstring language = (jstring)env->CallStaticObjectMethod(helper, method);
    const char *chars = env->GetStringUTFChars(language, nullptr);
    bool ok = strcmp(chars, "en") == 0;
    env->ReleaseStringUTFChars(language, chars);
    jstring made = env->NewStringUTF("round trip");
    const char *back = env->GetStringUTFChars(made, nullptr);
    ok = ok && strcmp(back, "round trip") == 0;
    return ok ? JNI_VERSION_1_4 : -2;
}
