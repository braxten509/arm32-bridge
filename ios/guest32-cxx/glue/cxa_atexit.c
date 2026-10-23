/* __cxa_atexit and __cxa_finalize, as Apple's libSystem has them (the
 * Itanium C++ ABI's static destructors). Games import them; so does this
 * library. Pure guest code: registrations stay here, and the runner's exit()
 * calls __cxa_finalize(NULL) when it wants destructors to run. */
#include <pthread.h>
#include <stdlib.h>

#define EXPORT __attribute__((visibility("default")))

struct entry {
    void (*function)(void *);
    void *argument;
    void *dso;
    int done;
};

struct block {
    struct block *older;
    int used;
    struct entry entries[32];
};

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static struct block *newest;

EXPORT int __cxa_atexit(void (*function)(void *), void *argument, void *dso) {
    pthread_mutex_lock(&lock);
    if (!newest || newest->used == 32) {
        struct block *block = calloc(1, sizeof *block);
        if (!block) {
            pthread_mutex_unlock(&lock);
            return -1;
        }
        block->older = newest;
        newest = block;
    }
    newest->entries[newest->used++] = (struct entry){function, argument, dso, 0};
    pthread_mutex_unlock(&lock);
    return 0;
}

/* Runs the not-yet-run entries of `dso` (all of them for NULL), newest
 * first, each once. As Apple's Libc atexit.c: an entry is claimed under the
 * lock, then called with the lock released, so a destructor may register
 * more (they run too) or wait for a thread that does. Entries never move:
 * blocks are only added. */
EXPORT void __cxa_finalize(void *dso) {
    pthread_mutex_lock(&lock);
    for (;;) {
        struct entry *next = NULL;
        for (struct block *block = newest; block && !next; block = block->older)
            for (int i = block->used - 1; i >= 0 && !next; i--)
                if (!block->entries[i].done && (!dso || block->entries[i].dso == dso))
                    next = &block->entries[i];
        if (!next)
            break;
        next->done = 1;
        void (*function)(void *) = next->function;
        void *argument = next->argument;
        pthread_mutex_unlock(&lock);
        function(argument);
        pthread_mutex_lock(&lock);
    }
    pthread_mutex_unlock(&lock);
}
