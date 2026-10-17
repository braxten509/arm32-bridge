// Stand-ins for the game's dlopen, android_dlopen_ext and dlsym calls.
//
// Android's loader picks which library namespace a call works in from the
// caller's address. The stand-ins pass the game's own return address on
// through the loader's __loader_* entry points (exported by ld-android.so), so
// a hooked call behaves exactly like the game's. Where those entry points are
// missing (the emulator's ARM translator), the game's own functions are called.
#ifdef __ANDROID__
#include <android/dlext.h>
#else
// Builds on the PC too, for the unit tests.
typedef struct android_dlextinfo android_dlextinfo;
#endif
#include <dlfcn.h>
#include <stddef.h>

typedef void *(*dlopen_fn)(const char *, int);
typedef void *(*android_dlopen_ext_fn)(const char *, int, const android_dlextinfo *);
typedef void *(*dlsym_fn)(void *, const char *);

// The game's own functions, set by hooks.rs before the stand-ins are used.
void *gamebridge_real_dlopen;
void *gamebridge_real_android_dlopen_ext;
void *gamebridge_real_dlsym;

static void *(*loader_dlopen)(const char *, int, const void *);
static void *(*loader_android_dlopen_ext)(const char *, int, const android_dlextinfo *, const void *);
static void *(*loader_dlsym)(void *, const char *, const void *);

// In Rust (hooks.rs): patch libraries that were just loaded; swap a looked-up
// function for its stand-in when there is one.
void gamebridge_after_load(void);
void *gamebridge_swap_symbol(const char *name, void *found);

void gamebridge_loader_start(void) {
    void *libdl = dlopen("libdl.so", RTLD_NOW | RTLD_NOLOAD);
    if (libdl == NULL) return;
    // All three or none: mixing caller-aware and plain calls would be confusing to debug.
    void *open = dlsym(libdl, "__loader_dlopen");
    void *open_ext = dlsym(libdl, "__loader_android_dlopen_ext");
    void *symbol = dlsym(libdl, "__loader_dlsym");
    if (open != NULL && open_ext != NULL && symbol != NULL) {
        loader_dlopen = open;
        loader_android_dlopen_ext = open_ext;
        loader_dlsym = symbol;
    }
}

int gamebridge_loader_passes_caller(void) {
    return loader_dlopen != NULL;
}

__attribute__((visibility("default"))) void *gamebridge_dlopen(const char *name, int flags) {
    const void *caller = __builtin_return_address(0);
    void *handle = loader_dlopen != NULL ? loader_dlopen(name, flags, caller)
                                         : ((dlopen_fn)gamebridge_real_dlopen)(name, flags);
    if (handle != NULL) gamebridge_after_load();
    return handle;
}

__attribute__((visibility("default"))) void *gamebridge_android_dlopen_ext(const char *name, int flags,
                                                                           const android_dlextinfo *info) {
    const void *caller = __builtin_return_address(0);
    void *handle = loader_android_dlopen_ext != NULL
                       ? loader_android_dlopen_ext(name, flags, info, caller)
                       : ((android_dlopen_ext_fn)gamebridge_real_android_dlopen_ext)(name, flags, info);
    if (handle != NULL) gamebridge_after_load();
    return handle;
}

__attribute__((visibility("default"))) void *gamebridge_dlsym(void *handle, const char *name) {
    const void *caller = __builtin_return_address(0);
    void *found = loader_dlsym != NULL ? loader_dlsym(handle, name, caller)
                                       : ((dlsym_fn)gamebridge_real_dlsym)(handle, name);
    if (found == NULL || name == NULL) return found;
    return gamebridge_swap_symbol(name, found);
}
