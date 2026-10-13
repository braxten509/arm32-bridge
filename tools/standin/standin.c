// A 64-bit stand-in for each of a game's 32-bit libraries. One prebuilt copy
// serves every game: it is packed as lib<name>.so, with the original 32-bit
// library next to it as lib<name>-arm32.so and the bridge as
// libarm32_bridge.so.
//
// When Java loads it, it hands its original to the bridge, which runs it.
// If the bridge can't, it falls back to switching the library off: it reads
// the class names the original library mentions, finds those classes' native
// methods through reflection, and registers a "do nothing" implementation
// (returns 0 / false / null) for each, so Java code that calls them keeps
// working with that feature off. Methods the bridge already connected to real
// 32-bit code are left alone.
#include <dlfcn.h>
#include <fcntl.h>
#include <jni.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <android/log.h>

#define TAG "arm32bridge"
#define log(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

// Returns 0 in both the integer and the floating point result register,
// whatever the method's return type (x0/d0; rax/xmm0 in the emulator copy).
void standin_zero(void);
#if defined(__aarch64__)
__asm__(".text\n"
        ".p2align 2\n"
        ".hidden standin_zero\n"
        ".type standin_zero, %function\n"
        "standin_zero:\n"
        "  mov x0, #0\n"
        "  fmov d0, xzr\n"
        "  ret\n");
#elif defined(__x86_64__)
__asm__(".text\n"
        ".p2align 4\n"
        ".hidden standin_zero\n"
        ".type standin_zero, @function\n"
        "standin_zero:\n"
        "  xor %eax, %eax\n"
        "  pxor %xmm0, %xmm0\n"
        "  ret\n");
#else
#error "stand-in: unsupported CPU"
#endif

typedef int (*connected_fn)(const char *class_name, const char *method, const char *signature);
// Returns NULL once the original runs, or why it couldn't.
typedef const char *(*load_fn)(JavaVM *vm, JNIEnv *env, const char *original);

#define BRIDGE_FILE "libarm32_bridge.so"

static jmethodID class_get_name, class_get_loader, get_declared_methods, method_get_name, get_modifiers,
    get_parameter_types, get_return_type;

static int clear(JNIEnv *env) {
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionClear(env);
        return 1;
    }
    return 0;
}

static int load_reflection(JNIEnv *env) {
    jclass class = (*env)->FindClass(env, "java/lang/Class");
    jclass method = (*env)->FindClass(env, "java/lang/reflect/Method");
    if (clear(env) || !class || !method) return 0;
    class_get_name = (*env)->GetMethodID(env, class, "getName", "()Ljava/lang/String;");
    class_get_loader = (*env)->GetMethodID(env, class, "getClassLoader", "()Ljava/lang/ClassLoader;");
    get_declared_methods = (*env)->GetMethodID(env, class, "getDeclaredMethods", "()[Ljava/lang/reflect/Method;");
    method_get_name = (*env)->GetMethodID(env, method, "getName", "()Ljava/lang/String;");
    get_modifiers = (*env)->GetMethodID(env, method, "getModifiers", "()I");
    get_parameter_types = (*env)->GetMethodID(env, method, "getParameterTypes", "()[Ljava/lang/Class;");
    get_return_type = (*env)->GetMethodID(env, method, "getReturnType", "()Ljava/lang/Class;");
    return !clear(env);
}

// Appends the JNI type descriptor of a java.lang.Class to `out` (size `size`).
static void append_descriptor(JNIEnv *env, jobject class, char *out, size_t size) {
    static const char *primitives[][2] = {{"boolean", "Z"}, {"byte", "B"},  {"char", "C"},   {"short", "S"}, {"int", "I"},
                                          {"long", "J"},    {"float", "F"}, {"double", "D"}, {"void", "V"}};
    jstring name = (*env)->CallObjectMethod(env, class, class_get_name);
    const char *text = (*env)->GetStringUTFChars(env, name, NULL);
    size_t used = strlen(out);
    const char *primitive = NULL;
    for (size_t i = 0; i < sizeof primitives / sizeof *primitives; i++)
        if (strcmp(text, primitives[i][0]) == 0) primitive = primitives[i][1];
    if (primitive)
        snprintf(out + used, size - used, "%s", primitive);
    else
        snprintf(out + used, size - used, text[0] == '[' ? "%s" : "L%s;", text);
    for (char *c = out + used; *c; c++)
        if (*c == '.') *c = '/';
    (*env)->ReleaseStringUTFChars(env, name, text);
    (*env)->DeleteLocalRef(env, name);
}

// Registers the do-nothing implementation for the native methods of `class_name`.
// Returns the number registered.
static int stub_class(JNIEnv *env, const char *class_name, connected_fn connected) {
    const jint native = 0x100;
    int count = 0;
    (*env)->PushLocalFrame(env, 64);
    jclass class = (*env)->FindClass(env, class_name);
    // Only the app's own classes: never the phone's built-in ones (no class loader).
    if (clear(env) || !class || !(*env)->CallObjectMethod(env, class, class_get_loader)) goto done;
    jobjectArray methods = (*env)->CallObjectMethod(env, class, get_declared_methods);
    if (clear(env) || !methods) goto done;
    for (jsize i = 0; i < (*env)->GetArrayLength(env, methods); i++) {
        jobject method = (*env)->GetObjectArrayElement(env, methods, i);
        if (!((*env)->CallIntMethod(env, method, get_modifiers) & native)) {
            (*env)->DeleteLocalRef(env, method);
            continue;
        }
        char signature[1024] = "(";
        jobjectArray types = (*env)->CallObjectMethod(env, method, get_parameter_types);
        for (jsize j = 0; j < (*env)->GetArrayLength(env, types); j++) {
            jobject type = (*env)->GetObjectArrayElement(env, types, j);
            append_descriptor(env, type, signature, sizeof signature);
            (*env)->DeleteLocalRef(env, type);
        }
        strncat(signature, ")", sizeof signature - strlen(signature) - 1);
        jobject returned = (*env)->CallObjectMethod(env, method, get_return_type);
        append_descriptor(env, returned, signature, sizeof signature);
        jstring name = (*env)->CallObjectMethod(env, method, method_get_name);
        const char *name_text = (*env)->GetStringUTFChars(env, name, NULL);
        if (!connected || !connected(class_name, name_text, signature)) {
            JNINativeMethod stub = {name_text, signature, (void *)standin_zero};
            if ((*env)->RegisterNatives(env, class, &stub, 1) == 0) count++;
            clear(env);
        }
        (*env)->ReleaseStringUTFChars(env, name, name_text);
        (*env)->DeleteLocalRef(env, name);
        (*env)->DeleteLocalRef(env, returned);
        (*env)->DeleteLocalRef(env, types);
        (*env)->DeleteLocalRef(env, method);
    }
done:
    clear(env);
    (*env)->PopLocalFrame(env, NULL);
    return count;
}

static int is_class_char(char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '$' ||
           c == '/';
}

JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void)reserved;
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_6) != JNI_OK) return JNI_VERSION_1_6;
    Dl_info info;
    if (!dladdr((void *)JNI_OnLoad, &info) || !info.dli_fname) return JNI_VERSION_1_6;
    char original[4096], directory[4096];
    snprintf(original, sizeof original, "%s", info.dli_fname);
    snprintf(directory, sizeof directory, "%s", info.dli_fname);
    char *slash = strrchr(directory, '/');
    if (slash) *slash = 0;
    size_t length = strlen(original);
    if (length > 3 && strcmp(original + length - 3, ".so") == 0) original[length - 3] = 0;
    strncat(original, "-arm32.so", sizeof original - strlen(original) - 1);

    int fd = open(original, O_RDONLY | O_CLOEXEC);
    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0 || st.st_size == 0) {
        log("[standin] %s: no original library at %s; nothing registered", info.dli_fname, original);
        if (fd >= 0) close(fd);
        return JNI_VERSION_1_6;
    }

    char bridge_path[4096];
    snprintf(bridge_path, sizeof bridge_path, "%s/" BRIDGE_FILE, directory);
    void *bridge = dlopen(bridge_path, RTLD_NOW);
    load_fn load = bridge ? (load_fn)dlsym(bridge, "arm32_bridge_load") : NULL;
    if (load) {
        const char *error = load(vm, env, original);
        if (!error) {
            close(fd);
            return JNI_VERSION_1_6;
        }
        log("[standin] %s: the bridge couldn't run it (%s); switching it off instead", info.dli_fname, error);
    } else {
        log("[standin] %s: no bridge at %s (%s); switching it off instead", info.dli_fname, bridge_path, dlerror());
    }
    connected_fn connected = bridge ? (connected_fn)dlsym(bridge, "arm32_bridge_connected") : NULL;

    const char *data = mmap(NULL, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
    close(fd);
    if (data == MAP_FAILED || !load_reflection(env)) return JNI_VERSION_1_6;

    // Every run of class-name characters that looks like a/b/C is tried once.
    int classes = 0, methods = 0;
    char name[512];
    for (off_t start = 0; start < st.st_size;) {
        off_t end = start;
        while (end < st.st_size && is_class_char(data[end])) end++;
        size_t size = end - start;
        if (size >= 6 && size < sizeof name && data[start] != '/' && data[end - 1] != '/' &&
            memchr(data + start, '/', size) && !memmem(data + start, size, "//", 2)) {
            memcpy(name, data + start, size);
            name[size] = 0;
            int stubbed = stub_class(env, name, connected);
            if (stubbed) classes++;
            methods += stubbed;
        }
        start = end + 1;
    }
    munmap((void *)data, st.st_size);
    log("[standin] %s: %d do-nothing native methods in %d classes", info.dli_fname, methods, classes);
    return JNI_VERSION_1_6;
}
