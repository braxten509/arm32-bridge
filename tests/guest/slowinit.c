// A test library whose start-up code takes a while: a thread that opens it
// while another thread is still running that code must wait for it (dlopen).
#include <dlfcn.h>
#include <unistd.h>

static volatile int ready = 0;
static void *self = 0;
__attribute__((constructor)) static void start(void) {
    usleep(200 * 1000);
    // Opening itself again from its own start-up code must not wait forever.
    self = dlopen("libslowinit.so", RTLD_NOW);
    ready = 1;
}

// 1 once the start-up code above has finished, 2 if it could reopen itself too.
int slowinit_ready(void) { return ready + (self != 0); }
