// Self-test for the bridge: each check exercises one part (loader,
// calling convention, libc stand-ins, guest callbacks, setjmp, C++
// exceptions, the pretend Java side). run_all() returns the failure count.
#include <android/log.h>
#include <jni.h>
#include <math.h>
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <time.h>
#include <semaphore.h>
#include <setjmp.h>
#include <unistd.h>
#include <zlib.h>
#include <dlfcn.h>
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
static pthread_mutex_t counter_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t counter_changed = PTHREAD_COND_INITIALIZER;
static int counter = 0;
static void *add_to_counter(void *amount) {
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&counter_lock);
        counter += (int)(long)amount;
        pthread_cond_broadcast(&counter_changed);
        pthread_mutex_unlock(&counter_lock);
        if (i % 100 == 0) sched_yield();
    }
    return (void *)(long)((int)(long)amount * 10);
}
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

    {
        float x = 0, y = 0;
        int r[4] = {0};
        long long big = 0;
        double d = 0;
        char word[16] = "", rest[16] = "";
        unsigned char small = 0;
        int got = sscanf("12.5,-3.25", "%f,%f", &x, &y);
        got += sscanf("{1, 2, 30, 400}", "{%d,%d,%d,%d}", &r[0], &r[1], &r[2], &r[3]);
        got += sscanf("big=12345678901 0x1f 2.5e3 hello [a-b]", "big=%lld %hhx %lf %s %[^]]", &big, &small, &d, word, rest);
        check(got == 11 && x == 12.5f && y == -3.25f && r[0] == 1 && r[3] == 400 && big == 12345678901LL &&
                  small == 0x1f && d == 2500.0 && strcmp(word, "hello") == 0 && strcmp(rest, "[a-b") == 0 &&
                  sscanf("", "%d", &r[0]) == EOF && sscanf("abc", "%d", &r[0]) == 0,
              "sscanf with 32-bit sizes");
    }
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

    pthread_t threads[4];
    for (long i = 0; i < 4; i++) pthread_create(&threads[i], nullptr, add_to_counter, (void *)(i + 1));
    pthread_mutex_lock(&counter_lock);
    while (counter < 10000) pthread_cond_wait(&counter_changed, &counter_lock);
    pthread_mutex_unlock(&counter_lock);
    long results = 0;
    for (int i = 0; i < 4; i++) {
        void *result = nullptr;
        pthread_join(threads[i], &result);
        results += (long)result;
    }
    check(counter == 10000 && results == 100, "4 threads + mutex + condition variable + join");

    pthread_mutexattr_t attributes;
    pthread_mutexattr_init(&attributes);
    pthread_mutexattr_settype(&attributes, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_t recursive;
    pthread_mutex_init(&recursive, &attributes);
    bool nested = pthread_mutex_lock(&recursive) == 0 && pthread_mutex_lock(&recursive) == 0 &&
                  pthread_mutex_unlock(&recursive) == 0 && pthread_mutex_unlock(&recursive) == 0 &&
                  pthread_mutex_trylock(&recursive) == 0;
    pthread_mutex_unlock(&recursive);
    check(nested, "recursive mutex");

    pthread_mutex_lock(&counter_lock);
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_nsec += 20 * 1000 * 1000;
    if (deadline.tv_nsec >= 1000000000) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000; }
    int waited = pthread_cond_timedwait(&counter_changed, &counter_lock, &deadline);
    pthread_mutex_unlock(&counter_lock);
    check(waited == ETIMEDOUT, "condition wait times out");

    {
        // The Linux kuser helpers old ARMv5 (armeabi) code calls at fixed addresses.
        typedef int (*cmpxchg_t)(int old_value, int new_value, volatile int *pointer);
        typedef int (*cmpxchg64_t)(const long long *old_value, const long long *new_value, volatile long long *pointer);
        typedef void *(*get_tls_t)();
        typedef void (*barrier_t)();
        volatile int word = 5;
        volatile long long wide = 1LL << 40;
        long long old_wide = 1LL << 40, new_wide = 3LL << 40, wrong = 7;
        bool ok = ((cmpxchg_t)0xffff0fc0)(5, 9, &word) == 0 && word == 9 && ((cmpxchg_t)0xffff0fc0)(5, 1, &word) != 0 &&
                  word == 9 && ((cmpxchg64_t)0xffff0f60)(&old_wide, &new_wide, &wide) == 0 && wide == new_wide &&
                  ((cmpxchg64_t)0xffff0f60)(&wrong, &old_wide, &wide) != 0 && wide == new_wide &&
                  *(volatile int *)0xffff0ffc >= 3;
        void *tls;
        __asm__ volatile("mrc p15, 0, %0, c13, c0, 3" : "=r"(tls));
        ((barrier_t)0xffff0fa0)();
        check(ok && ((get_tls_t)0xffff0fe0)() == tls, "kernel helpers (cmpxchg, cmpxchg64, barrier, get_tls)");
    }

    sem_t semaphore;
    sem_init(&semaphore, 0, 1);
    int semaphore_ok = sem_wait(&semaphore) == 0 && sem_trywait(&semaphore) == -1 && errno == EAGAIN &&
                       sem_post(&semaphore) == 0 && sem_trywait(&semaphore) == 0;
    sem_destroy(&semaphore);
    check(semaphore_ok, "semaphores");

    char directory[512];
    check(getcwd(directory, sizeof directory) == directory && directory[0] == '/', "getcwd");

    {
        const char message[] = "zlib round trip zlib round trip zlib round trip";
        unsigned char packed[256], unpacked[256];
        uLongf packed_size = sizeof packed, unpacked_size = sizeof unpacked;
        bool ok = compress(packed, &packed_size, (const Bytef *)message, sizeof message) == Z_OK &&
                  uncompress(unpacked, &unpacked_size, packed, packed_size) == Z_OK &&
                  unpacked_size == sizeof message && memcmp(unpacked, message, sizeof message) == 0 &&
                  crc32(0, (const Bytef *)"123456789", 9) == 0xCBF43926;
        check(ok, "zlib compress / uncompress / crc32 (built-in 32-bit libz.so)");
    }

    {
        // Game code opening another of the game's libraries (libsecond.so, built next to this one).
        void *second = dlopen("libsecond.so", RTLD_NOW);
        typedef int (*value_fn)(void);
        typedef jint (*onload_fn)(JavaVM *, void *);
        value_fn value = second ? (value_fn)dlsym(second, "second_value") : nullptr;
        onload_fn onload = second ? (onload_fn)dlsym(second, "JNI_OnLoad") : nullptr;
        check(second && value && value() == 42, "dlopen loads a game library and runs its start-up code");
        check(onload && onload(nullptr, nullptr) == 0x7777, "dlsym on a library's handle finds that library's JNI_OnLoad");
        check(dlopen("libsecond.so", RTLD_NOW) == second, "dlopen of an open library returns the same handle");
        void *phone = dlopen("libGLESv2.so", RTLD_NOW);
        check(phone && dlsym(phone, "glClear"), "dlopen of a phone library still finds its functions");
    }

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
