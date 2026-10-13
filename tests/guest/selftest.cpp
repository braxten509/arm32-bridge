// Self-test for the bridge: each check exercises one part (loader,
// calling convention, libc stand-ins, guest callbacks, setjmp, C++
// exceptions, the pretend Java side). run_all() returns the failure count.
#include <android/log.h>
#include <jni.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <math.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <time.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <ucontext.h>
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
// Mono's way of pausing a thread: a pause signal whose handler answers on a
// semaphore, then waits in sigsuspend for the resume signal.
static sem_t paused_answer, release_worker;
static volatile int resumed_flag = 0, handler_thread_ok = 0, context_ok = 0, pauses = 0;
static pthread_t worker_thread;
static void on_resume(int, siginfo_t *, void *) { resumed_flag = 1; }
static void on_pause(int signal, siginfo_t *info, void *context) {
    ucontext_t *uc = (ucontext_t *)context;
    handler_thread_ok = pthread_equal(pthread_self(), worker_thread);
    context_ok = info->si_signo == signal && uc->uc_mcontext.arm_sp != 0 && uc->uc_mcontext.arm_pc != 0;
    pauses++;
    sigset_t wait_mask;
    sigfillset(&wait_mask);
    sigdelset(&wait_mask, SIGXCPU);
    // Clear before answering: once answered, the resume signal can come at any moment.
    resumed_flag = 0;
    sem_post(&paused_answer);
    while (!resumed_flag) sigsuspend(&wait_mask);
    sem_post(&paused_answer);
}
static void *worker(void *) {
    sem_wait(&release_worker);
    return nullptr;
}
static volatile int handoff_flag;
static void *set_handoff_flag(void *) {
    handoff_flag = 1;
    return nullptr;
}
__attribute__((target("arm"), noinline)) static bool spin_arm(void) {
    for (long spins = 0; spins < 100000000 && !handoff_flag; spins++) __asm__ volatile("yield");
    return handoff_flag;
}
__attribute__((target("thumb"), noinline)) static bool spin_thumb(void) {
    for (long spins = 0; spins < 100000000 && !handoff_flag; spins++) __asm__ volatile("yield");
    return handoff_flag;
}
static bool spin_calling(void) {
    for (long spins = 0; spins < 3000000 && !handoff_flag; spins++) (void)getpid();
    return handoff_flag;
}
// A loop with no YIELD and no phone function: only the translator's time slicing frees the lock.
static bool spin_plain(void) {
    for (long spins = 0; spins < 400000000 && !handoff_flag; spins++) {
    }
    return handoff_flag;
}
static bool spin_until_other_thread_runs(bool (*spin)(void)) {
    pthread_t thread;
    handoff_flag = 0;
    if (pthread_create(&thread, nullptr, set_handoff_flag, nullptr) != 0) return false;
    bool seen = spin();
    pthread_join(thread, nullptr);
    return seen;
}

static volatile int ran_after_exit, exit_destructor_value;
static pthread_key_t exit_key;
static void on_exit_key(void *value) { exit_destructor_value = (int)(long)value; }
static int exit_inside_compare(const void *, const void *) {
    pthread_exit((void *)77);
    return 0;
}
static void *exits_from_nested_call(void *) {
    pthread_setspecific(exit_key, (void *)5);
    int numbers[3] = {3, 1, 2};
    qsort(numbers, 3, sizeof numbers[0], exit_inside_compare);
    ran_after_exit = 1;
    return (void *)1;
}
// Many short threads one after another: each finished thread's stack must be
// reused (the guest stack area only fits about 250 at once).
static void *note_ran(void *ran) {
    ++*(int *)ran;
    return __builtin_frame_address(0); // where this thread's stack is
}
static bool many_threads_reuse_stacks() {
    int ran = 0, places = 0;
    static uintptr_t seen[300];
    for (int i = 0; i < 300; i++) {
        pthread_t thread;
        void *stack;
        if (pthread_create(&thread, nullptr, note_ran, &ran) != 0 || pthread_join(thread, &stack) != 0 || !stack)
            return false;
        uintptr_t where = (uintptr_t)stack >> 20; // the thread's 1 MiB stack
        bool known = false;
        for (int j = 0; j < places; j++) known = known || seen[j] == where;
        if (!known) seen[places++] = where;
    }
    __android_log_print(ANDROID_LOG_INFO, TAG, "many threads: %d of 300 ran, on %d stacks", ran, places);
    // Without reuse every thread has its own stack; with it a few do (a joiner can
    // start the next thread just before the last one's stack is handed back).
    return ran == 300 && places < 150;
}
static bool thread_exits_with_value(void) {
    ran_after_exit = exit_destructor_value = 0;
    pthread_key_create(&exit_key, on_exit_key);
    pthread_t thread;
    if (pthread_create(&thread, nullptr, exits_from_nested_call, nullptr) != 0) return false;
    void *result = nullptr;
    pthread_join(thread, &result);
    return result == (void *)77 && !ran_after_exit && exit_destructor_value == 5;
}

static volatile int sleeper_started, sleeper_signalled;
static void on_sleeper_signal(int) { sleeper_signalled = 1; }
static void *long_sleeper(void *) {
    sleeper_started = 1;
    usleep(3000000);
    return nullptr;
}
static double seconds_now(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec + now.tv_nsec / 1e9;
}
static bool signal_reaches_sleeping_thread(void) {
    struct sigaction action = {};
    action.sa_handler = on_sleeper_signal;
    sigaction(SIGUSR2, &action, nullptr);
    sleeper_started = sleeper_signalled = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, long_sleeper, nullptr) != 0) return false;
    while (!sleeper_started) usleep(1000);
    usleep(20000);
    double sent = seconds_now();
    pthread_kill(thread, SIGUSR2);
    while (!sleeper_signalled && seconds_now() - sent < 1.0) usleep(1000);
    bool quick = sleeper_signalled && seconds_now() - sent < 0.2;
    pthread_detach(thread);
    return quick;
}

// ---- guest threads running at the same time ----
static bool bridge_flag(const char *name) {
    int (*flag)(void) = (int (*)(void))dlsym(RTLD_DEFAULT, name);
    return flag && flag();
}
static const int kRaceThreads = 4;
static int kRaceAdds = 100000;
static volatile int race_gate;
static volatile unsigned race32;
static volatile unsigned long long race64;
static volatile int race_swp_lock;
static int race_swp;
static volatile int race_kuser;
static volatile long long race_kuser64;
static int race_locked;
static pthread_mutex_t race_lock = PTHREAD_MUTEX_INITIALIZER;
typedef int (*kuser_cmpxchg_fn)(int old_value, int new_value, volatile int *pointer);
typedef int (*kuser_cmpxchg64_fn)(const long long *old_value, const long long *new_value, volatile long long *pointer);
// A spinlock built on SWP (ARMv5 code still uses it; ARM mode only).
__attribute__((target("arm"), noinline)) static void swp_lock_add(void) {
    int old_value;
    do {
        __asm__ volatile(".arch armv6\n\tswp %0, %1, [%2]\n\t.arch armv7-a"
                         : "=&r"(old_value)
                         : "r"(1), "r"(&race_swp_lock)
                         : "memory");
    } while (old_value != 0);
    __asm__ volatile("dmb ish" ::: "memory");
    race_swp++;
    __asm__ volatile("dmb ish" ::: "memory");
    race_swp_lock = 0;
}
#define RACE_ADDER(mode)                                                                               \
    __attribute__((target(#mode), noinline)) static void *race_adder_##mode(void *) {                  \
        kuser_cmpxchg_fn cmpxchg = (kuser_cmpxchg_fn)0xffff0fc0;                                       \
        kuser_cmpxchg64_fn cmpxchg64 = (kuser_cmpxchg64_fn)0xffff0f60;                                 \
        while (!race_gate) {                                                                           \
        }                                                                                              \
        for (int i = 0; i < kRaceAdds; i++) {                                                          \
            __atomic_fetch_add(&race32, 1, __ATOMIC_RELAXED);                                          \
            __atomic_fetch_add(&race64, 1, __ATOMIC_RELAXED);                                          \
            int old_value;                                                                             \
            do old_value = race_kuser;                                                                 \
            while (cmpxchg(old_value, old_value + 1, &race_kuser) != 0);                               \
            long long old_wide, new_wide;                                                              \
            do {                                                                                       \
                old_wide = race_kuser64;                                                               \
                new_wide = old_wide + 1;                                                               \
            } while (cmpxchg64(&old_wide, &new_wide, &race_kuser64) != 0);                             \
            pthread_mutex_lock(&race_lock);                                                            \
            race_locked++;                                                                             \
            pthread_mutex_unlock(&race_lock);                                                          \
            swp_lock_add();                                                                            \
        }                                                                                              \
        return nullptr;                                                                                \
    }
RACE_ADDER(arm)
RACE_ADDER(thumb)
// Four threads (two ARM, two Thumb) add to shared counters at once; every total must be exact.
static bool shared_counters_exact(void) {
    race_gate = 0;
    race32 = 0;
    race64 = 0;
    race_swp_lock = race_swp = 0;
    race_kuser = 0;
    race_kuser64 = 0;
    race_locked = 0;
    pthread_t threads[kRaceThreads];
    for (int i = 0; i < kRaceThreads; i++)
        if (pthread_create(&threads[i], nullptr, i % 2 ? race_adder_thumb : race_adder_arm, nullptr) != 0) return false;
    race_gate = 1;
    for (int i = 0; i < kRaceThreads; i++) pthread_join(threads[i], nullptr);
    const unsigned total = kRaceThreads * kRaceAdds;
    __android_log_print(ANDROID_LOG_INFO, TAG, "counters: %u %llu kuser %d %lld mutex %d swp %d (want %u)", race32,
                        race64, race_kuser, race_kuser64, race_locked, race_swp, total);
    return race32 == total && race64 == total && race_kuser == (int)total && race_kuser64 == (long long)total &&
           race_locked == (int)total && race_swp == (int)total;
}

// 8- and 16-bit atomics: a total that wraps could hide lost adds, so each round
// stays below 256 and is checked exactly, with untouched bytes on both sides.
static int kSmallRounds = 1000;
static const int kSmallAdds = 60;
static struct alignas(8) {
    unsigned char canary_a, value8, canary_b, canary_c;
    unsigned short value16, canary_d;
} small;
static volatile int small_round, small_done;
#define SMALL_ADDER(mode)                                                                              \
    __attribute__((target(#mode), noinline)) static void *small_adder_##mode(void *) {                 \
        for (int round = 1; round <= kSmallRounds; round++) {                                          \
            while (__atomic_load_n(&small_round, __ATOMIC_ACQUIRE) != round) sched_yield();            \
            for (int i = 0; i < kSmallAdds; i++) {                                                     \
                __atomic_fetch_add(&small.value8, 1, __ATOMIC_RELAXED);                                \
                __atomic_fetch_add(&small.value16, 1, __ATOMIC_RELAXED);                               \
            }                                                                                          \
            __atomic_fetch_add(&small_done, 1, __ATOMIC_RELEASE);                                      \
        }                                                                                              \
        return nullptr;                                                                                \
    }
SMALL_ADDER(arm)
SMALL_ADDER(thumb)
static bool small_counters_exact(void) {
    small_round = small_done = 0;
    pthread_t threads[kRaceThreads];
    for (int i = 0; i < kRaceThreads; i++)
        if (pthread_create(&threads[i], nullptr, i % 2 ? small_adder_thumb : small_adder_arm, nullptr) != 0) return false;
    int bad_rounds = 0;
    for (int round = 1; round <= kSmallRounds; round++) {
        small.canary_a = small.canary_b = small.canary_c = 0xA5;
        small.canary_d = 0x5AA5;
        small.value8 = 0;
        small.value16 = 0;
        __atomic_store_n(&small_done, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&small_round, round, __ATOMIC_RELEASE);
        while (__atomic_load_n(&small_done, __ATOMIC_ACQUIRE) != kRaceThreads) sched_yield();
        bool ok = small.value8 == kRaceThreads * kSmallAdds && small.value16 == kRaceThreads * kSmallAdds &&
                  small.canary_a == 0xA5 && small.canary_b == 0xA5 && small.canary_c == 0xA5 && small.canary_d == 0x5AA5;
        if (!ok && bad_rounds++ < 3)
            __android_log_print(ANDROID_LOG_INFO, TAG, "small counters round %d: %d %d canaries %x %x %x %x", round,
                                small.value8, small.value16, small.canary_a, small.canary_b, small.canary_c, small.canary_d);
    }
    for (int i = 0; i < kRaceThreads; i++) pthread_join(threads[i], nullptr);
    return bad_rounds == 0;
}

static volatile unsigned busy_sink;
static void *busy_work(void *) {
    unsigned x = 1;
    for (int i = 0; i < 60000000; i++) x = x * 1103515245u + 12345u;
    busy_sink = x;
    return nullptr;
}
static double run_busy_threads(int count) {
    pthread_t threads[4];
    double started = seconds_now();
    for (int i = 0; i < count; i++) pthread_create(&threads[i], nullptr, busy_work, nullptr);
    for (int i = 0; i < count; i++) pthread_join(threads[i], nullptr);
    return seconds_now() - started;
}
// Four busy threads finish in well under four times what one takes alone
// (best of three runs each; the phone's cores differ in speed, and the
// guest cannot pick them).
static bool busy_threads_overlap(void) {
    double one = 1e9, four = 1e9;
    for (int run = 0; run < 3; run++) {
        double took = run_busy_threads(1);
        if (took < one) one = took;
        took = run_busy_threads(4);
        if (took < four) four = took;
    }
    __android_log_print(ANDROID_LOG_INFO, TAG, "busy threads: one %.0f ms, four %.0f ms", one * 1000, four * 1000);
    return four < 0.75 * 4 * one;
}

// Two threads open a library whose start-up code takes 200 ms; both must get
// it only once that code has finished (and the code may reopen its own library).
static int opened_ready[2];
static void *open_slow_library(void *slot) {
    void *library = dlopen("libslowinit.so", RTLD_NOW);
    int (*ready)(void) = library ? (int (*)(void))dlsym(library, "slowinit_ready") : nullptr;
    opened_ready[(long)slot] = ready ? ready() : -1;
    return nullptr;
}
static bool dlopen_waits_for_start_up(void) {
    pthread_t threads[2];
    for (long i = 0; i < 2; i++) pthread_create(&threads[i], nullptr, open_slow_library, (void *)i);
    for (int i = 0; i < 2; i++) pthread_join(threads[i], nullptr);
    __android_log_print(ANDROID_LOG_INFO, TAG, "slow start-up library: %d %d (want 2 2)", opened_ready[0], opened_ready[1]);
    return opened_ready[0] == 2 && opened_ready[1] == 2;
}

// Ping-pong between two threads; a lost wake-up shows up as a 5 s timeout.
static const int kPingRounds = 20000;
static pthread_mutex_t ping_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ping_changed = PTHREAD_COND_INITIALIZER;
static int ping_turn;
static volatile int ping_timeouts;
static sem_t ping_a, ping_b;
static void deadline_in(struct timespec *deadline, int seconds) {
    clock_gettime(CLOCK_REALTIME, deadline);
    deadline->tv_sec += seconds;
}
static void *ping_partner(void *) {
    for (int i = 0; i < kPingRounds && !ping_timeouts; i++) {
        pthread_mutex_lock(&ping_lock);
        while (ping_turn != 1 && !ping_timeouts) {
            struct timespec deadline;
            deadline_in(&deadline, 5);
            if (pthread_cond_timedwait(&ping_changed, &ping_lock, &deadline) == ETIMEDOUT) ping_timeouts++;
        }
        ping_turn = 0;
        pthread_cond_signal(&ping_changed);
        pthread_mutex_unlock(&ping_lock);
    }
    for (int i = 0; i < kPingRounds && !ping_timeouts; i++) {
        struct timespec deadline;
        deadline_in(&deadline, 5);
        if (sem_timedwait(&ping_a, &deadline) != 0) ping_timeouts++;
        sem_post(&ping_b);
    }
    return nullptr;
}
static bool ping_pong_without_lost_wakeups(void) {
    ping_turn = 0;
    ping_timeouts = 0;
    sem_init(&ping_a, 0, 0);
    sem_init(&ping_b, 0, 0);
    pthread_t partner;
    if (pthread_create(&partner, nullptr, ping_partner, nullptr) != 0) return false;
    for (int i = 0; i < kPingRounds && !ping_timeouts; i++) {
        pthread_mutex_lock(&ping_lock);
        while (ping_turn != 0 && !ping_timeouts) {
            struct timespec deadline;
            deadline_in(&deadline, 5);
            if (pthread_cond_timedwait(&ping_changed, &ping_lock, &deadline) == ETIMEDOUT) ping_timeouts++;
        }
        ping_turn = 1;
        pthread_cond_signal(&ping_changed);
        pthread_mutex_unlock(&ping_lock);
    }
    for (int i = 0; i < kPingRounds && !ping_timeouts; i++) {
        sem_post(&ping_a);
        struct timespec deadline;
        deadline_in(&deadline, 5);
        if (sem_timedwait(&ping_b, &deadline) != 0) ping_timeouts++;
    }
    pthread_join(partner, nullptr);
    return ping_timeouts == 0;
}

static pthread_once_t race_once = PTHREAD_ONCE_INIT;
static volatile int race_once_runs, race_once_finished, race_once_early;
static void race_once_init(void) {
    __atomic_fetch_add(&race_once_runs, 1, __ATOMIC_SEQ_CST);
    usleep(20000);
    __atomic_store_n(&race_once_finished, 1, __ATOMIC_SEQ_CST);
}
static void *race_once_caller(void *) {
    while (!race_gate) {
    }
    pthread_once(&race_once, race_once_init);
    if (race_once_runs != 1 || !__atomic_load_n(&race_once_finished, __ATOMIC_SEQ_CST)) race_once_early = 1;
    return nullptr;
}
// pthread_once called by four threads at once runs the function once; nobody returns before it ends.
static bool once_from_four_threads(void) {
    race_gate = 0;
    race_once_runs = race_once_finished = race_once_early = 0;
    pthread_t threads[kRaceThreads];
    for (int i = 0; i < kRaceThreads; i++)
        if (pthread_create(&threads[i], nullptr, race_once_caller, nullptr) != 0) return false;
    race_gate = 1;
    for (int i = 0; i < kRaceThreads; i++) pthread_join(threads[i], nullptr);
    return race_once_runs == 1 && !race_once_early;
}

static volatile int spinner_running, spinner_signalled;
static void on_spinner_signal(int) { spinner_signalled = 1; }
static void *busy_spinner(void *) {
    spinner_running = 1;
    for (long spins = 0; spins < 2000000000 && !spinner_signalled; spins++) {
    }
    return nullptr;
}
// A signal reaches a thread busy in a loop that makes no calls.
static bool signal_reaches_spinning_thread(void) {
    struct sigaction action = {};
    action.sa_handler = on_spinner_signal;
    sigaction(SIGUSR2, &action, nullptr);
    spinner_running = spinner_signalled = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, busy_spinner, nullptr) != 0) return false;
    while (!spinner_running) usleep(1000);
    usleep(20000);
    double sent = seconds_now();
    pthread_kill(thread, SIGUSR2);
    pthread_join(thread, nullptr);
    return spinner_signalled && seconds_now() - sent < 0.5;
}

// ---- threads running at the same time: waits, code changes, signals ----

static long futex_call(volatile int *word, int op, int value, const struct timespec *timeout) {
    return syscall(SYS_futex, word, op, value, timeout, nullptr, 0);
}
static volatile int futex_word, futex_waiters_in;
static void *futex_waiter(void *) {
    __sync_fetch_and_add(&futex_waiters_in, 1);
    while (futex_word == 0) futex_call(&futex_word, FUTEX_WAIT, 0, nullptr);
    return nullptr;
}
static volatile int race_word, race_waits_done, race_woken;
static void *timed_futex_waiter(void *) {
    struct timespec brief = {0, 1000000};
    for (int i = 0; i < 1000; i++)
        if (futex_call(&race_word, FUTEX_WAIT, 0, &brief) == 0) race_woken++;
    race_waits_done = 1;
    return nullptr;
}
// Three sleepers: WAKE 2 reports 2, the next WAKE 2 reports 1. Then 1000
// timed waits race wakes: every wake that reports a sleeper woke one wait.
static bool futex_wake_counts(void) {
    futex_word = futex_waiters_in = 0;
    pthread_t threads[3];
    for (int i = 0; i < 3; i++)
        if (pthread_create(&threads[i], nullptr, futex_waiter, nullptr) != 0) return false;
    while (futex_waiters_in < 3) usleep(1000);
    usleep(50000); // all three asleep
    futex_word = 1;
    long first = futex_call(&futex_word, FUTEX_WAKE, 2, nullptr);
    usleep(20000);
    long second = futex_call(&futex_word, FUTEX_WAKE, 2, nullptr);
    for (int i = 0; i < 3; i++) pthread_join(threads[i], nullptr);

    race_word = race_waits_done = race_woken = 0;
    pthread_t waiter;
    if (pthread_create(&waiter, nullptr, timed_futex_waiter, nullptr) != 0) return false;
    long wakes = 0;
    while (!race_waits_done) wakes += futex_call(&race_word, FUTEX_WAKE, 1, nullptr);
    pthread_join(waiter, nullptr);
    __android_log_print(ANDROID_LOG_INFO, TAG, "futex wakes: %ld then %ld (want 2 then 1); raced: %ld wakes, %d woken",
                        first, second, wakes, race_woken);
    return first == 2 && second == 1 && wakes == race_woken;
}

// Code in writable + executable memory, rewritten while other threads run it
// (two call it in a loop, one spins on a flag without calls, then calls it once).
typedef int (*made_function)(void);
static made_function made_code;
static volatile int made_stop, made_release, made_seen_new[2], made_spinner_got;
static void *call_made_code(void *slot) {
    while (!made_stop)
        if (made_code() == 2) made_seen_new[(long)slot] = 1;
    return nullptr;
}
static void *spin_then_call_made_code(void *) {
    while (!made_release) {
    }
    made_spinner_got = made_code();
    return nullptr;
}
static void write_made_code(unsigned *code, unsigned value) {
    code[0] = 0xE3A00000u | value; // mov r0, #value
    code[1] = 0xE12FFF1Eu;          // bx lr
    __builtin___clear_cache((char *)code, (char *)(code + 2));
}
static bool code_rewrite_across_threads(void) {
    unsigned *code = (unsigned *)mmap(nullptr, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) return false;
    write_made_code(code, 1);
    made_code = (made_function)code;
    made_stop = made_release = made_spinner_got = 0;
    made_seen_new[0] = made_seen_new[1] = 0;
    bool first = made_code() == 1;
    pthread_t threads[3];
    for (long i = 0; i < 2; i++)
        if (pthread_create(&threads[i], nullptr, call_made_code, (void *)i) != 0) return false;
    if (pthread_create(&threads[2], nullptr, spin_then_call_made_code, nullptr) != 0) return false;
    usleep(30000);
    write_made_code(code, 2);
    double started = seconds_now();
    while (!(made_seen_new[0] && made_seen_new[1]) && seconds_now() - started < 2.0) usleep(1000);
    made_release = 1;
    made_stop = 1;
    for (int i = 0; i < 3; i++) pthread_join(threads[i], nullptr);
    bool seen = made_seen_new[0] && made_seen_new[1] && made_spinner_got == 2 && made_code() == 2;
    munmap(code, 4096);
    return first && seen;
}

// A signal reaches a thread spinning inside a qsort comparator (game code
// called back from a phone function).
static volatile int comparator_spinning, comparator_signalled;
static void on_comparator_signal(int) { comparator_signalled = 1; }
static int spinning_comparator(const void *a, const void *b) {
    comparator_spinning = 1;
    for (long spins = 0; spins < 2000000000 && !comparator_signalled; spins++) {
    }
    return *(const int *)a - *(const int *)b;
}
static void *sort_with_spinning_comparator(void *) {
    int values[2] = {2, 1};
    qsort(values, 2, sizeof(int), spinning_comparator);
    return nullptr;
}
static bool signal_reaches_callback(void) {
    struct sigaction action = {};
    action.sa_handler = on_comparator_signal;
    sigaction(SIGUSR2, &action, nullptr);
    comparator_spinning = comparator_signalled = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, sort_with_spinning_comparator, nullptr) != 0) return false;
    while (!comparator_spinning) usleep(1000);
    usleep(20000);
    double sent = seconds_now();
    pthread_kill(thread, SIGUSR2);
    pthread_join(thread, nullptr);
    return comparator_signalled && seconds_now() - sent < 0.5;
}

// A handler reading thread-specific data while its thread keeps changing it.
static pthread_key_t tsd_key;
static volatile int tsd_stop, tsd_bad, tsd_handled, tsd_running;
static void on_tsd_signal(int) {
    long value = (long)pthread_getspecific(tsd_key);
    if ((value & 1) != 1) tsd_bad++;
    tsd_handled++;
}
static void *tsd_looper(void *) {
    pthread_setspecific(tsd_key, (void *)1); // valid before the first signal can come
    tsd_running = 1;
    for (long i = 0; !tsd_stop; i++) {
        long value = (i << 1) | 1;
        pthread_setspecific(tsd_key, (void *)value);
        if ((long)pthread_getspecific(tsd_key) != value) tsd_bad++;
    }
    return nullptr;
}
static bool handler_reads_thread_data(void) {
    struct sigaction action = {};
    action.sa_handler = on_tsd_signal;
    sigaction(SIGUSR2, &action, nullptr);
    if (pthread_key_create(&tsd_key, nullptr) != 0) return false;
    tsd_stop = tsd_bad = tsd_handled = tsd_running = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, tsd_looper, nullptr) != 0) return false;
    while (!tsd_running) usleep(1000);
    for (int i = 0; i < 200; i++) {
        pthread_kill(thread, SIGUSR2);
        usleep(500);
    }
    usleep(20000);
    tsd_stop = 1;
    pthread_join(thread, nullptr);
    pthread_key_delete(tsd_key);
    return tsd_bad == 0 && tsd_handled > 0;
}

// One thread makes short phone calls in a loop; 1000 signals sent one at a
// time each run their handler exactly once.
static volatile int calls_stop, calls_running, calls_handled;
static void on_calls_signal(int) { calls_handled++; }
static void *phone_call_looper(void *) {
    calls_running = 1;
    struct timespec now;
    while (!calls_stop) clock_gettime(CLOCK_MONOTONIC, &now);
    return nullptr;
}
static bool signals_during_phone_calls(void) {
    struct sigaction action = {};
    action.sa_handler = on_calls_signal;
    sigaction(SIGUSR2, &action, nullptr);
    calls_stop = calls_running = calls_handled = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, phone_call_looper, nullptr) != 0) return false;
    while (!calls_running) usleep(1000);
    bool in_time = true;
    for (int i = 0; i < 1000 && in_time; i++) {
        pthread_kill(thread, SIGUSR2);
        double sent = seconds_now();
        while (calls_handled < i + 1 && (in_time = seconds_now() - sent < 1.0)) {
        }
    }
    usleep(20000);
    calls_stop = 1;
    pthread_join(thread, nullptr);
    __android_log_print(ANDROID_LOG_INFO, TAG, "signals during phone calls: %d handled (want 1000)", calls_handled);
    return in_time && calls_handled == 1000;
}

// Signals reach a thread blocked in a phone function (read() on an empty
// pipe) while the read stays blocked, and the read finishes normally later.
static int blocked_pipe[2];
static volatile int blocked_reading, blocked_read_done, blocked_first, blocked_second;
static volatile long blocked_read_result;
static void on_blocked_first(int) { blocked_first++; }
static void on_blocked_second(int) { blocked_second++; }
static void *blocked_reader(void *) {
    char got[4] = {0};
    blocked_reading = 1;
    long result;
    do result = read(blocked_pipe[0], got, sizeof got);
    while (result < 0 && errno == EINTR);
    blocked_read_result = result == 4 && memcmp(got, "data", 4) == 0 ? 4 : -1;
    blocked_read_done = 1;
    return nullptr;
}
static bool signals_reach_blocked_read(void) {
    struct sigaction first = {}, second = {};
    first.sa_handler = on_blocked_first;
    second.sa_handler = on_blocked_second;
    sigaction(SIGUSR1, &first, nullptr);
    sigaction(SIGUSR2, &second, nullptr);
    if (pipe(blocked_pipe) != 0) return false;
    blocked_reading = blocked_read_done = blocked_first = blocked_second = 0;
    blocked_read_result = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, blocked_reader, nullptr) != 0) return false;
    while (!blocked_reading) usleep(1000);
    usleep(50000);
    double sent = seconds_now();
    pthread_kill(thread, SIGUSR1);
    pthread_kill(thread, SIGUSR2);
    while (!(blocked_first && blocked_second) && seconds_now() - sent < 1.0) usleep(1000);
    double took = seconds_now() - sent;
    bool still_blocked = !blocked_read_done;
    usleep(50000);
    bool once_each = blocked_first == 1 && blocked_second == 1;
    write(blocked_pipe[1], "data", 4);
    pthread_join(thread, nullptr);
    close(blocked_pipe[0]);
    close(blocked_pipe[1]);
    __android_log_print(ANDROID_LOG_INFO, TAG, "blocked read: handlers %d %d after %.0f ms, read still blocked %d, read %ld",
                        blocked_first, blocked_second, took * 1000, still_blocked, blocked_read_result);
    return once_each && took < 0.1 && still_blocked && blocked_read_result == 4;
}

// ---- GL state follows the GL context, not the thread ----

static EGLDisplay gl_display;
static EGLSurface gl_surface;
static EGLContext gl_first, gl_shared;
static GLuint gl_pack_buffer, gl_mapped_buffer;
static volatile int gl_pack_ok, gl_unmap_ok;

// Thread 2, first context: the pack buffer bound on thread 1 still counts,
// so the pointer is an offset in it.
static void *read_pixel_into_bound_buffer(void *) {
    eglMakeCurrent(gl_display, gl_surface, gl_surface, gl_first);
    // Offset 4, not 0: taken as a guest address instead, it would become a
    // large host address (no pixel), where offset 0 would become null.
    glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, (void *)4);
    const unsigned char *pixel = (const unsigned char *)glMapBufferRange(GL_PIXEL_PACK_BUFFER, 4, 4, GL_MAP_READ_BIT);
    gl_pack_ok = pixel && pixel[0] == 255 && pixel[1] == 0 && pixel[2] == 0 && pixel[3] == 255;
    if (pixel) glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
    eglMakeCurrent(gl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    return nullptr;
}

// Thread 2, the sharing context: unmapping uploads what thread 1 wrote.
static void *unmap_from_shared_context(void *) {
    eglMakeCurrent(gl_display, gl_surface, gl_surface, gl_shared);
    glBindBuffer(GL_ARRAY_BUFFER, gl_mapped_buffer);
    bool unmapped = glUnmapBuffer(GL_ARRAY_BUFFER) == GL_TRUE;
    const unsigned char *data = (const unsigned char *)glMapBufferRange(GL_ARRAY_BUFFER, 0, 4, GL_MAP_READ_BIT);
    gl_unmap_ok = unmapped && data && data[0] == 0x11 && data[1] == 0x22 && data[2] == 0x33 && data[3] == 0x44;
    if (data) glUnmapBuffer(GL_ARRAY_BUFFER);
    eglMakeCurrent(gl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    return nullptr;
}

// 1 = passed, 0 = failed, -1 = no GL context can be made here (not runnable).
static int gl_state_follows_context(void) {
    gl_display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (gl_display == EGL_NO_DISPLAY || !eglInitialize(gl_display, nullptr, nullptr)) return -1;
    const EGLint config_wanted[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT_KHR,
                                    EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig config;
    EGLint found = 0;
    if (!eglChooseConfig(gl_display, config_wanted, &config, 1, &found) || found != 1) return -1;
    const EGLint surface_size[] = {EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE};
    const EGLint version[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
    gl_surface = eglCreatePbufferSurface(gl_display, config, surface_size);
    gl_first = eglCreateContext(gl_display, config, EGL_NO_CONTEXT, version);
    gl_shared = eglCreateContext(gl_display, config, gl_first, version);
    if (gl_surface == EGL_NO_SURFACE || gl_first == EGL_NO_CONTEXT || gl_shared == EGL_NO_CONTEXT) return -1;

    // Thread 1 (this one): red pixel, a pack buffer bound, and a buffer left mapped with data written.
    eglMakeCurrent(gl_display, gl_surface, gl_surface, gl_first);
    glClearColor(1, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glGenBuffers(1, &gl_pack_buffer);
    glBindBuffer(GL_PIXEL_PACK_BUFFER, gl_pack_buffer);
    glBufferData(GL_PIXEL_PACK_BUFFER, 64, nullptr, GL_STREAM_READ);
    glGenBuffers(1, &gl_mapped_buffer);
    glBindBuffer(GL_ARRAY_BUFFER, gl_mapped_buffer);
    glBufferData(GL_ARRAY_BUFFER, 16, nullptr, GL_DYNAMIC_DRAW);
    unsigned char *mapped = (unsigned char *)glMapBufferRange(GL_ARRAY_BUFFER, 0, 4, GL_MAP_WRITE_BIT);
    if (mapped) {
        mapped[0] = 0x11; mapped[1] = 0x22; mapped[2] = 0x33; mapped[3] = 0x44;
    }
    glFinish();
    eglMakeCurrent(gl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);

    gl_pack_ok = gl_unmap_ok = 0;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, read_pixel_into_bound_buffer, nullptr) != 0) return 0;
    pthread_join(thread, nullptr);
    if (pthread_create(&thread, nullptr, unmap_from_shared_context, nullptr) != 0) return 0;
    pthread_join(thread, nullptr);

    eglMakeCurrent(gl_display, gl_surface, gl_surface, gl_first);
    glDeleteBuffers(1, &gl_pack_buffer);
    glDeleteBuffers(1, &gl_mapped_buffer);
    eglMakeCurrent(gl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(gl_display, gl_shared);
    eglDestroyContext(gl_display, gl_first);
    eglDestroySurface(gl_display, gl_surface);
    __android_log_print(ANDROID_LOG_INFO, TAG, "gl state: pack binding on another thread %d, unmap from shared context %d",
                        gl_pack_ok, gl_unmap_ok);
    return mapped && gl_pack_ok && gl_unmap_ok ? 1 : 0;
}

#define MULTIPLY_TESTS(mode)                                                                         \
    __attribute__((target(#mode), noinline)) static unsigned long long umaal_##mode(unsigned lo, unsigned hi, unsigned n, unsigned m) { \
        __asm__ volatile("umaal %0, %1, %2, %3" : "+r"(lo), "+r"(hi) : "r"(n), "r"(m));                     \
        return (unsigned long long)hi << 32 | lo;                                                    \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static int smmul_##mode(int n, int m) {                 \
        int d;                                                                                       \
        __asm__ volatile("smmul %0, %1, %2" : "=r"(d) : "r"(n), "r"(m));                              \
        return d;                                                                                    \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static int smmulr_##mode(int n, int m) {                \
        int d;                                                                                       \
        __asm__ volatile("smmulr %0, %1, %2" : "=r"(d) : "r"(n), "r"(m));                             \
        return d;                                                                                    \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static int smmla_##mode(int n, int m, int a) {         \
        int d;                                                                                       \
        __asm__ volatile("smmla %0, %1, %2, %3" : "=r"(d) : "r"(n), "r"(m), "r"(a));                  \
        return d;                                                                                    \
    }                                                                                                \
    static bool multiplies_##mode(void) {                                                            \
        return umaal_##mode(0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu) == 0xFFFFFFFFFFFFFFFFull && \
               umaal_##mode(5, 7, 0x10000, 0x10000) == 0x10000000Cull &&                             \
               smmul_##mode(0x40000000, 6) == 1 && smmul_##mode(-0x40000000, 6) == -2 &&            \
               smmulr_##mode(0x40000000, 6) == 2 && smmulr_##mode(0x20000000, 6) == 1 &&            \
               smmla_##mode(0x40000000, 6, 100) == 101 && smmla_##mode(-0x40000000, 6, 100) == 98; \
    }
MULTIPLY_TESTS(arm)
MULTIPLY_TESTS(thumb)

// Halfword multiplies; SMLA sets the sticky Q flag when the sum overflows.
#define HALFWORD_TESTS(mode)                                                                         \
    __attribute__((target(#mode), noinline)) static unsigned smlatb_##mode(int n, int m, int a, int *result) { \
        unsigned flags;                                                                              \
        int d;                                                                                       \
        __asm__ volatile("msr APSR_nzcvq, %[zero]\n"                                                \
                         "smlatb %[d], %[n], %[m], %[a]\n"                                           \
                         "mrs %[f], APSR\n"                                                          \
                         : [d] "=&r"(d), [f] "=&r"(flags)                                            \
                         : [n] "r"(n), [m] "r"(m), [a] "r"(a), [zero] "r"(0)                         \
                         : "cc");                                                                    \
        *result = d;                                                                                 \
        return (flags >> 27) & 1;                                                                    \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static int smulbt_##mode(int n, int m) {               \
        int d;                                                                                       \
        __asm__ volatile("smulbt %0, %1, %2" : "=r"(d) : "r"(n), "r"(m));                             \
        return d;                                                                                    \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static unsigned pack_##mode(unsigned n, unsigned m, unsigned *top) { \
        unsigned bottom_top, top_bottom;                                                             \
        __asm__ volatile("pkhbt %0, %2, %3, lsl #16\n"                                              \
                         "pkhtb %1, %2, %3, asr #16\n"                                              \
                         : "=&r"(bottom_top), "=&r"(top_bottom)                                      \
                         : "r"(n), "r"(m));                                                          \
        *top = top_bottom;                                                                           \
        return bottom_top;                                                                           \
    }                                                                                                \
    static bool halfword_multiplies_##mode(void) {                                                   \
        int small = 0, big = 0;                                                                      \
        bool quiet = smlatb_##mode(0x00050000, 3, 10, &small) == 0;                                  \
        bool sticky = smlatb_##mode(0x7FFF0000, 2, 0x7FFFFFFF, &big) == 1;                           \
        unsigned packed_top = 0;                                                                     \
        bool pack_ok = pack_##mode(0x1111AAAA, 0x8000BBBB, &packed_top) == 0xBBBBAAAA &&             \
                       packed_top == 0x11118000;                                                     \
        return quiet && small == 25 && sticky && big == (int)0x8000FFFD &&                           \
               smulbt_##mode(0x0000FFFE, 0x00070000) == -14 && pack_ok;                              \
    }
HALFWORD_TESTS(arm)
HALFWORD_TESTS(thumb)

// What neon_permutes_* must produce from the bytes 0..31 (see the instructions there).
static void neon_permute_expected(unsigned char *want) {
    unsigned char *w = want;
    for (int i = 0; i < 16; i++) *w++ = (unsigned char)(4 + i);                       // vext.32 #1
    static const int rev[4] = {1, 0, 3, 2};                                              // vrev64.32
    for (int word = 0; word < 4; word++)
        for (int b = 0; b < 4; b++) *w++ = (unsigned char)(rev[word] * 4 + b);
    static const int trn[8] = {0, 4, 2, 6, 1, 5, 3, 7};                                  // vtrn.32
    for (int word = 0; word < 8; word++)
        for (int b = 0; b < 4; b++) *w++ = (unsigned char)(trn[word] * 4 + b);
    for (int half = 0; half < 8; half++) {                                               // vzip.16
        int from = (half % 2 == 0 ? 0 : 16) + (half / 2) * 2;
        w[half * 2] = (unsigned char)from;
        w[half * 2 + 1] = (unsigned char)(from + 1);
        int high = (half % 2 == 0 ? 8 : 24) + (half / 2) * 2;
        w[16 + half * 2] = (unsigned char)high;
        w[16 + half * 2 + 1] = (unsigned char)(high + 1);
    }
    w += 32;
    for (int i = 0; i < 8; i++) *w++ = (unsigned char)(2 * i);                           // vuzp.8 even
    for (int i = 0; i < 8; i++) *w++ = (unsigned char)(2 * i + 1);                       // vuzp.8 odd
    for (int i = 0; i < 4; i++) { *w++ = 14; *w++ = 15; }                                // vdup.16 d17[3]
    for (int i = 0; i < 4; i++) {                                                        // vneg.s16
        unsigned short value = (unsigned short)(0 - ((2 * i) | ((2 * i + 1) << 8)));
        *w++ = (unsigned char)value;
        *w++ = (unsigned char)(value >> 8);
    }
    for (int i = 0; i < 8; i++) *w++ = (unsigned char)(i == 3 || i == 7 ? i ^ 0x80 : i); // vneg.f32 (after vswp)
    for (int i = 0; i < 8; i++) *w++ = (unsigned char)~i;                                // vmvn (after vswp)
}

// NEON lanes, widening, conversions and immediates (the translator must match Unicorn).
#define NEON_TESTS(mode)                                                                             \
    __attribute__((target(#mode), noinline)) static void neon_lanes_##mode(unsigned a, unsigned b, const unsigned short *p, unsigned *out) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vmov.i32 d20, #0\n"                                                        \
                         "vmov.i32 d21, #0\n"                                                        \
                         "vmov.16 d20[0], %[a]\n"                                                    \
                         "vmov.16 d20[1], %[b]\n"                                                    \
                         "vld1.16 {d20[2]}, [%[p]]\n"                                                \
                         "add %[p], %[p], #2\n"                                                      \
                         "vld1.16 {d20[3]}, [%[p]]!\n"                                               \
                         "vmov.8 d21[5], %[a]\n"                                                     \
                         "vld1.8 {d21[1]}, [%[p]]\n"                                                 \
                         "vst1.32 {d20, d21}, [%[out]]\n"                                            \
                         : [p] "+r"(p)                                                               \
                         : [a] "r"(a), [b] "r"(b), [out] "r"(out)                                    \
                         : "d20", "d21", "memory");                                                  \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static void neon_to_core_##mode(unsigned lo, unsigned hi, int *out) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vmov d21, %[lo], %[hi]\n"                                                  \
                         "vmov.s16 %[lo], d21[1]\n"                                                  \
                         "str %[lo], [%[out]]\n"                                                     \
                         "vmov.u16 %[lo], d21[3]\n"                                                  \
                         "str %[lo], [%[out], #4]\n"                                                 \
                         "vmov.s8 %[lo], d21[2]\n"                                                   \
                         "str %[lo], [%[out], #8]\n"                                                 \
                         "vmov.u8 %[lo], d21[7]\n"                                                   \
                         "str %[lo], [%[out], #12]\n"                                                \
                         "vmov.32 %[lo], d21[1]\n"                                                   \
                         "str %[lo], [%[out], #16]\n"                                                \
                         : [lo] "+r"(lo)                                                             \
                         : [hi] "r"(hi), [out] "r"(out)                                              \
                         : "d21", "memory");                                                         \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static void neon_widen_##mode(const short *in, float *scaled, unsigned *wide) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vld1.16 {d18}, [%[in]]\n"                                                  \
                         "vmovl.s16 q10, d18\n"                                                      \
                         "vmovl.u16 q11, d18\n"                                                      \
                         "vst1.32 {d22, d23}, [%[wide]]\n"                                           \
                         "vcvt.f32.s32 q10, q10\n"                                                   \
                         "vmov.i32 q8, #0x38000000\n"                                                \
                         "vmul.f32 q10, q10, q8\n"                                                   \
                         "vst1.32 {d20, d21}, [%[scaled]]\n"                                         \
                         :                                                                           \
                         : [in] "r"(in), [scaled] "r"(scaled), [wide] "r"(wide)                      \
                         : "d16", "d17", "d18", "d20", "d21", "d22", "d23", "memory");               \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static void neon_float_##mode(const float *in, unsigned *out) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vld1.32 {d16, d17}, [%[in]]\n"                                             \
                         "vcvt.s32.f32 q9, q8\n"                                                     \
                         "vst1.32 {d18, d19}, [%[out]]!\n"                                           \
                         "vcvt.u32.f32 q9, q8\n"                                                     \
                         "vst1.32 {d18, d19}, [%[out]]!\n"                                           \
                         "vmov.f32 q10, #0.5\n"                                                      \
                         "vmax.f32 q11, q8, q10\n"                                                   \
                         "vst1.32 {d22, d23}, [%[out]]!\n"                                           \
                         "vmin.f32 d22, d16, d20\n"                                                  \
                         "vst1.32 {d22}, [%[out]]\n"                                                 \
                         : [out] "+r"(out)                                                           \
                         : [in] "r"(in)                                                              \
                         : "d16", "d17", "d18", "d19", "d20", "d21", "d22", "d23", "memory");        \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static void neon_immediates_##mode(const unsigned short *h, unsigned *out) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vmvn.i32 d24, #0xff00\n"                                                   \
                         "vbic.i32 d24, #0xff\n"                                                     \
                         "vmov.i64 d25, #0xff00ff0000ff00ff\n"                                       \
                         "vorr.i16 d25, #0x100\n"                                                    \
                         "vld1.16 {d26[]}, [%[h]]\n"                                                 \
                         "vshll.s16 q14, d26, #3\n"                                                  \
                         "vst1.32 {d24, d25}, [%[out]]!\n"                                           \
                         "vst1.32 {d26}, [%[out]]!\n"                                                \
                         "vst1.32 {d28, d29}, [%[out]]\n"                                            \
                         : [out] "+r"(out)                                                           \
                         : [h] "r"(h)                                                                \
                         : "d24", "d25", "d26", "d28", "d29", "memory");                             \
    }                                                                                                \
    __attribute__((target(#mode), noinline)) static void neon_permutes_##mode(const unsigned char *in, unsigned char *out) { \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vld1.8 {d16, d17, d18, d19}, [%[in]]\n"                                    \
                         "vext.32 q10, q8, q9, #1\n"                                                 \
                         "vrev64.32 q11, q8\n"                                                       \
                         "vorr q12, q8, q8\n"                                                        \
                         "vorr q13, q9, q9\n"                                                        \
                         "vtrn.32 q12, q13\n"                                                        \
                         "vorr q14, q8, q8\n"                                                        \
                         "vorr q15, q9, q9\n"                                                        \
                         "vzip.16 q14, q15\n"                                                        \
                         "vst1.8 {d20, d21, d22, d23}, [%[out]]!\n"                                  \
                         "vst1.8 {d24, d25, d26, d27}, [%[out]]!\n"                                  \
                         "vst1.8 {d28, d29, d30, d31}, [%[out]]!\n"                                  \
                         "vorr d4, d16, d16\n"                                                       \
                         "vorr d5, d17, d17\n"                                                       \
                         "vuzp.8 d4, d5\n"                                                           \
                         "vdup.16 d6, d17[3]\n"                                                      \
                         "vneg.s16 d7, d16\n"                                                        \
                         "vmvn d2, d16\n"                                                            \
                         "vneg.f32 d3, d16\n"                                                        \
                         "vswp d2, d3\n"                                                             \
                         "vst1.8 {d4, d5, d6, d7}, [%[out]]!\n"                                      \
                         "vst1.8 {d2, d3}, [%[out]]\n"                                               \
                         : [out] "+r"(out)                                                           \
                         : [in] "r"(in)                                                              \
                         : "d2", "d3", "d4", "d5", "d6", "d7", "d16", "d17", "d18", "d19", "d20",   \
                           "d21", "d22", "d23", "d24", "d25", "d26", "d27", "d28", "d29", "d30",    \
                           "d31", "memory");                                                         \
    }                                                                                                \
    static bool neon_extras_##mode(void) {                                                           \
        static const unsigned short halves[3] = {0x1111, 0x2222, 0x3344};                            \
        unsigned lanes[4];                                                                           \
        neon_lanes_##mode(0x1234ABCD, 0x5678, halves, lanes);                                         \
        bool lanes_ok = lanes[0] == 0x5678ABCD && lanes[1] == 0x22221111 && lanes[2] == 0x4400 &&     \
                        lanes[3] == 0xCD00;                                                          \
        int core[5];                                                                                 \
        neon_to_core_##mode(0x9ABC8001, 0xF0E1D2C3, core);                                           \
        bool core_ok = core[0] == -25924 && core[1] == 61665 && core[2] == -68 && core[3] == 240 &&  \
                       (unsigned)core[4] == 0xF0E1D2C3;                                              \
        static const short in[4] = {-2, 3, 32767, -32768};                                           \
        float scaled[4];                                                                             \
        unsigned wide[4];                                                                            \
        neon_widen_##mode(in, scaled, wide);                                                          \
        bool widen_ok = wide[0] == 0xFFFE && wide[1] == 3 && wide[2] == 0x7FFF && wide[3] == 0x8000 && \
                        scaled[0] == -2 / 32768.0f && scaled[1] == 3 / 32768.0f &&                   \
                        scaled[2] == 32767 / 32768.0f && scaled[3] == -1.0f;                          \
        static const float floats[4] = {-1.5f, 2.7f, 3e9f, -0.0f};                                    \
        unsigned converted[14];                                                                      \
        neon_float_##mode(floats, converted);                                                         \
        const float half = 0.5f, low = -1.5f, mid = 2.7f, huge = 3e9f;                               \
        unsigned half_bits, low_bits, mid_bits, huge_bits;                                           \
        memcpy(&half_bits, &half, 4);                                                                \
        memcpy(&low_bits, &low, 4);                                                                  \
        memcpy(&mid_bits, &mid, 4);                                                                  \
        memcpy(&huge_bits, &huge, 4);                                                                \
        const unsigned want[14] = {(unsigned)-1, 2, 0x7FFFFFFF, 0, 0, 2, 3000000000u, 0,             \
                                   half_bits, mid_bits, huge_bits, half_bits, low_bits, half_bits};  \
        bool float_ok = memcmp(converted, want, sizeof want) == 0;                                   \
        static const unsigned short minus_five = 0xFFFB;                                             \
        unsigned imm[8];                                                                             \
        neon_immediates_##mode(&minus_five, imm);                                                     \
        bool imm_ok = imm[0] == 0xFFFF0000 && imm[1] == 0xFFFF0000 && imm[2] == 0x01FF01FF &&         \
                      imm[3] == 0xFF00FF00 && imm[4] == 0xFFFBFFFB && imm[5] == 0xFFFBFFFB &&          \
                      imm[6] == (unsigned)-40 && imm[7] == (unsigned)-40;                            \
        unsigned char block[32], permuted[160], permute_want[160];                                   \
        for (int i = 0; i < 32; i++) block[i] = (unsigned char)i;                                    \
        neon_permutes_##mode(block, permuted);                                                       \
        neon_permute_expected(permute_want);                                                         \
        bool permute_ok = memcmp(permuted, permute_want, sizeof permuted) == 0;                      \
        if (!permute_ok)                                                                             \
            for (int i = 0; i < 160; i++)                                                            \
                if (permuted[i] != permute_want[i]) {                                                \
                    __android_log_print(ANDROID_LOG_INFO, TAG, #mode " permute byte %d: %d want %d", i, permuted[i], permute_want[i]); \
                    break;                                                                           \
                }                                                                                    \
        imm_ok = imm_ok && permute_ok;                                                               \
        if (!(lanes_ok && core_ok && widen_ok && float_ok && imm_ok))                                \
            __android_log_print(ANDROID_LOG_INFO, TAG, #mode " neon: lanes %d core %d widen %d float %d imm %d", \
                                lanes_ok, core_ok, widen_ok, float_ok, imm_ok);                      \
        return lanes_ok && core_ok && widen_ok && float_ok && imm_ok;                                \
    }
NEON_TESTS(arm)
NEON_TESTS(thumb)

// Runs many NEON instructions on mixed data; the self-test logs a checksum of the results so the
// translator (and ARM32_CHECK=all) can be compared with Unicorn (ARM32_ENGINE=unicorn).
#define NEON_COVERAGE(mode)                                                                         \
    __attribute__((target(#mode), noinline)) static unsigned neon_coverage_##mode(const unsigned char *in, const float *floats, \
                                                                              const unsigned char *data, unsigned seed, unsigned char *out) { \
        unsigned flags;                                                                             \
        __asm__ volatile(                                                                           \
                         ".fpu neon-vfpv4\n" \
                         "vld1.8 {d16, d17, d18, d19}, [%[in]]!\n" \
                         "vld1.8 {d20, d21}, [%[in]]!\n" \
                         "vld1.8 {d22, d23, d24, d25}, [%[floats]]\n" \
                         "vmov.i32 q0, #0\n" \
                         "vmov.i32 q1, #0\n" \
                         "vmov.i32 q2, #0\n" \
                         "vmov.i32 q3, #0\n" \
                         "vmsr fpscr, %[zero]\n" \
                         "vhadd.s8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqadd.u16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrhadd.s32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vhsub.u8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqsub.s16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcgt.s32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcge.u8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vshl.s16 q13, q8, q10\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqshl.u32 q13, q8, q10\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrshl.s8 q13, q8, q10\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqrshl.s16 q13, q8, q10\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vmax.u16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vmin.s8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vabd.u32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vaba.s16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vtst.8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vceq.i32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vmla.i16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vmls.i32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vmul.i8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vmul.p8 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqdmulh.s16 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqrdmulh.s32 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vfma.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vfms.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vabd.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vceq.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcge.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcgt.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vacge.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vacgt.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrecps.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrsqrts.f32 q13, q11, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vbsl q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vbit q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vbif q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorn q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vadd.i64 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqadd.s64 q13, q8, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vshl.s64 q13, q8, q10\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vpaddl.s8 q13, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpadal.u16 q13, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcls.s16 q13, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vclz.i32 q13, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcnt.8 q13, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqabs.s8 q13, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vqneg.s32 q13, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcgt.s16 q13, q8, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcge.s8 q13, q9, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vceq.i32 q13, q8, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcle.s16 q13, q9, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vclt.s32 q13, q8, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcgt.f32 q13, q11, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcge.f32 q13, q12, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vceq.f32 q13, q11, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vcle.f32 q13, q12, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vclt.f32 q13, q11, #0\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrecpe.f32 q13, q11\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrsqrte.f32 q13, q12\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrecpe.u32 q13, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vrsqrte.u32 q13, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpmax.s16 d26, d16, d19\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpmin.u8 d26, d17, d18\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpadd.i32 d26, d16, d17\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpadd.f32 d26, d22, d25\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpmax.f32 d26, d23, d24\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vpmin.f32 d26, d22, d23\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vmovn.i32 d26, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vqmovn.s16 d26, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vqmovn.u32 d26, q8\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vqmovun.s32 d26, q9\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vaba.u8 d26, d17, d19\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vorr q13, q10, q10\n" \
                         "vcgt.f32 d26, d23, d25\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vdup.8 q13, %[seed]\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vdup.16 d27, %[seed]\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vdup.32 q13, %[seed]\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vld2.16 {d26, d27}, [%[data]]\n" \
                         "vst1.8 {d26, d27}, [%[out]]!\n" \
                         "vld2.8 {d0, d2}, [%[data]]\n" \
                         "vst1.8 {d0, d1, d2, d3}, [%[out]]!\n" \
                         "vld2.32 {d4, d5, d6, d7}, [%[data]]\n" \
                         "vst1.8 {d4, d5, d6, d7}, [%[out]]!\n" \
                         "vld3.16 {d1, d2, d3}, [%[data]]\n" \
                         "vst1.8 {d0, d1, d2, d3}, [%[out]]!\n" \
                         "vld3.8 {d0, d2, d4}, [%[data]]\n" \
                         "vst1.8 {d0, d1, d2, d3}, [%[out]]!\n" \
                         "vst1.8 {d4}, [%[out]]!\n" \
                         "vld4.32 {d3, d4, d5, d6}, [%[data]]\n" \
                         "vst1.8 {d3, d4, d5, d6}, [%[out]]!\n" \
                         "vld4.8 {d0, d2, d4, d6}, [%[data]]\n" \
                         "vst1.8 {d0, d1, d2, d3}, [%[out]]!\n" \
                         "vst1.8 {d4, d5, d6, d7}, [%[out]]!\n" \
                         "vst2.16 {d16, d17}, [%[out]]!\n" \
                         "vst3.8 {d17, d19, d21}, [%[out]]!\n" \
                         "vst4.32 {d16, d17, d18, d19}, [%[out]]!\n" \
                         "vst2.32 {d16, d17, d18, d19}, [%[out]]!\n" \
                         "vmrs %[flags], fpscr\n" \
                         "vmsr fpscr, %[zero]\n" \
                         : [in] "+r"(in), [out] "+r"(out), [flags] "=&r"(flags)                      \
                         : [floats] "r"(floats), [data] "r"(data), [seed] "r"(seed), [zero] "r"(0)   \
                         : "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d16", "d17", "d18", "d19", \
                           "d20", "d21", "d22", "d23", "d24", "d25", "d26", "d27", "memory");        \
        return flags;                                                                               \
    }
NEON_COVERAGE(arm)
NEON_COVERAGE(thumb)

static unsigned char neon_coverage_out[4096], neon_coverage_previous[4096];
static unsigned neon_coverage_checksum(unsigned (*run)(const unsigned char *, const float *, const unsigned char *, unsigned, unsigned char *)) {
    static unsigned char in[48], data[64];
    unsigned char *out = neon_coverage_out;
    memcpy(neon_coverage_previous, neon_coverage_out, sizeof neon_coverage_out);
    static const float floats[8] = {1.5f, -2.25f, 0.0f, __builtin_inff(), 3e38f, 1e-40f, -0.1f, 7.0f};
    for (int i = 0; i < 48; i++) in[i] = (unsigned char)(i * 37 + 11) ^ (i & 8 ? 0x80 : 0);
    for (int i = 0; i < 64; i++) data[i] = (unsigned char)(i * 13 + 5);
    memset(out, 0, sizeof neon_coverage_out);
    unsigned flags = run(in, floats, data, 0x9ABCDEF5u, out);
    unsigned hash = 2166136261u ^ flags;
    for (unsigned i = 0; i < sizeof neon_coverage_out; i++) hash = (hash ^ out[i]) * 16777619u;
    return hash;
}

// A raw Linux system call (`svc 0`, number in r7), as Mono makes them.
#define SYSCALL_TESTS(mode)                                                                          \
    __attribute__((target(#mode), noinline)) static int raw_getpid_##mode(void) {                   \
        int result;                                                                                  \
        __asm__ volatile("push {r7}\n"                                                               \
                         "mov r7, #20\n"                                                             \
                         "svc #0\n"                                                                  \
                         "pop {r7}\n"                                                                \
                         "mov %0, r0\n"                                                              \
                         : "=r"(result)                                                              \
                         :                                                                           \
                         : "r0", "memory");                                                          \
        return result;                                                                               \
    }                                                                                                \
    /* A loop whose block ends in NEON mode and jumps back to VFP code: the VFP divide must */       \
    /* still use the guest's rounding mode (round toward zero here).                         */       \
    __attribute__((target(#mode), noinline)) static unsigned neon_then_vfp_##mode(void) {           \
        unsigned saved, mode_bits, result;                                                           \
        int count = 3;                                                                               \
        __asm__ volatile(".fpu neon\n"                                                               \
                         "vmrs %[saved], fpscr\n"                                                    \
                         "orr %[bits], %[saved], #0xC00000\n"                                        \
                         "vmsr fpscr, %[bits]\n"                                                     \
                         "vmov.f32 s12, #1.0\n"                                                      \
                         "vmov.f32 s13, #3.0\n"                                                      \
                         "vmov.i32 q8, #0\n"                                                         \
                         "1:\n"                                                                      \
                         "vdiv.f32 s14, s12, s13\n"                                                  \
                         "vmul.f32 q8, q8, q8\n"                                                     \
                         "subs %[count], %[count], #1\n"                                             \
                         "bne 1b\n"                                                                  \
                         "vmov %[result], s14\n"                                                     \
                         "vmsr fpscr, %[saved]\n"                                                    \
                         : [saved] "=&r"(saved), [bits] "=&r"(mode_bits), [result] "=&r"(result),    \
                           [count] "+r"(count)                                                       \
                         :                                                                           \
                         : "d6", "d7", "d16", "d17", "cc");                                          \
        return result;                                                                               \
    }
SYSCALL_TESTS(arm)
SYSCALL_TESTS(thumb)

// RSC (ARM only): imm - Rn - NOT(C).
__attribute__((target("arm"), noinline)) static bool reverse_subtract_with_carry(void) {
    int with_carry, without_carry, zero = 0, n = 30;
    __asm__ volatile("cmp %[z], #0\n"   // C = 1
                     "rsc %[a], %[n], #100\n"
                     "cmp %[z], #1\n"   // C = 0
                     "rsc %[b], %[n], #100\n"
                     : [a] "=&r"(with_carry), [b] "=&r"(without_carry)
                     : [n] "r"(n), [z] "r"(zero)
                     : "cc");
    return with_carry == 70 && without_carry == 69;
}

// STMDB sp!, {sp, lr}: the stored SP is the value before the push.
__attribute__((target("arm"), noinline)) static bool push_stack_pointer(void) {
    unsigned before, stored;
    __asm__ volatile("mov %[b], sp\n"
                     ".inst 0xe92d6000\n" // stmdb sp!, {sp, lr}
                     "ldr %[s], [sp]\n"
                     "add sp, sp, #8\n"
                     : [b] "=&r"(before), [s] "=&r"(stored)
                     :
                     : "memory");
    return before == stored;
}

// ARM code may store PC with STM (Mono's trampolines do): it reads as the STM + 8.
__attribute__((target("arm"), noinline)) static int stored_pc_offset(void) {
    int here, stored;
    __asm__ volatile("mov %[h], pc\n"
                     ".inst 0xe92d9000\n" // stmdb sp!, {r12, pc}
                     "ldr %[s], [sp, #4]\n"
                     "add sp, sp, #8\n"
                     : [h] "=&r"(here), [s] "=&r"(stored)
                     :
                     : "memory");
    return stored - here;
}

// MOVS with a shift amount in a register: an amount of 0 keeps the carry flag,
// any other amount sets it to the last bit shifted out (even if the result is 0).
__attribute__((target("arm"), noinline)) static unsigned shift_carry(unsigned value, unsigned amount) {
    unsigned carry;
    __asm__ volatile(
        "cmp %[one], #0\n"           // carry set
        "movs %[v], %[v], lsr %[a]\n"
        "mov %[c], #0\n"
        "movcs %[c], #1\n"
        : [v] "+r"(value), [c] "=&r"(carry)
        : [a] "r"(amount), [one] "r"(1)
        : "cc");
    return carry;
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
    {
        void *volatile twice = malloc(40);
        free(twice);
        free(twice);
        int on_stack = 0;
        void *volatile stack_address = &on_stack;
        free(stack_address);
        void *first = malloc(40), *second = malloc(40);
        check(first && second && first != second, "double free and stack free leave the heap intact");
        free(first);
        free(second);
    }

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
    check(thread_exits_with_value(), "pthread_exit from a nested call returns its value to join");
    check(many_threads_reuse_stacks(), "300 threads one after another (finished threads' stacks are reused)");

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

    {
        struct sigaction action = {};
        action.sa_sigaction = on_pause;
        action.sa_flags = SA_SIGINFO | SA_RESTART;
        // As Mono does: everything blocked in the handler, so a resume that comes
        // before its sigsuspend waits for it (else the resume can be missed).
        sigfillset(&action.sa_mask);
        sigaction(SIGPWR, &action, nullptr);
        action.sa_sigaction = on_resume;
        sigaction(SIGXCPU, &action, nullptr);
        struct sigaction read_back = {};
        sigaction(SIGPWR, nullptr, &read_back);
        sem_init(&paused_answer, 0, 0);
        sem_init(&release_worker, 0, 0);
        pthread_create(&worker_thread, nullptr, worker, nullptr);
        sched_yield();
        // Pause the worker while it waits in sem_wait, then resume it.
        int sent = pthread_kill(worker_thread, SIGPWR);
        sem_wait(&paused_answer);
        int was_paused = pauses == 1 && !resumed_flag;
        pthread_kill(worker_thread, SIGXCPU);
        sem_wait(&paused_answer);
        // A blocked signal waits until it is unblocked.
        sigset_t pause_only;
        sigemptyset(&pause_only);
        sigaddset(&pause_only, SIGUSR1);
        static volatile int usr1 = 0;
        struct sigaction user = {};
        user.sa_handler = [](int) { usr1++; };
        sigaction(SIGUSR1, &user, nullptr);
        pthread_sigmask(SIG_BLOCK, &pause_only, nullptr);
        raise(SIGUSR1);
        int held = usr1 == 0;
        pthread_sigmask(SIG_UNBLOCK, &pause_only, nullptr);
        sem_post(&release_worker);
        pthread_join(worker_thread, nullptr);
        check(read_back.sa_sigaction == on_pause && sent == 0 && was_paused && handler_thread_ok && context_ok &&
                  pauses == 1 && held && usr1 == 1,
              "signals: pause and resume another thread (Mono's garbage collector), blocked signals wait");
    }

    check(shift_carry(5, 0) == 1 && shift_carry(1, 2) == 0 && shift_carry(2, 1) == 0 && shift_carry(1, 1) == 1 &&
              shift_carry(0x80000000u, 32) == 1 && shift_carry(1, 33) == 0,
          "carry from a shift by a register amount (0 keeps it)");

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

    {
        int (*volatile abs_import)(int) = abs;
        long (*volatile labs_import)(long) = labs;
        long long (*volatile llabs_import)(long long) = llabs;
        check(abs_import(-7) == 7 && labs_import(-8) == 8 && llabs_import(-5000000000LL) == 5000000000LL,
              "abs / labs / llabs");

        char cpuinfo[4096] = {0};
        FILE *file = fopen("/proc/cpuinfo", "r");
        size_t got = file ? fread(cpuinfo, 1, sizeof cpuinfo - 1, file) : 0;
        if (file) fclose(file);
        int fd = open("/proc/cpuinfo", O_RDONLY);
        char first[16] = {0};
        bool read_ok = fd >= 0 && read(fd, first, 9) == 9 && strcmp(first, "processor") == 0;
        if (fd >= 0) close(fd);
        check(got > 0 && strstr(cpuinfo, "CPU architecture: 7") && strstr(cpuinfo, " neon ") &&
                  strstr(cpuinfo, " vfpv3 ") && !strstr(cpuinfo, " aes") && read_ok,
              "/proc/cpuinfo describes a 32-bit ARM CPU with NEON");

        int sized = open("/proc/cpuinfo", O_RDONLY);
        char head[16] = {0};
        off64_t end = sized >= 0 ? lseek64(sized, 0, SEEK_END) : -1;
        bool large_files = end > 9 && pread64(sized, head, 9, 0) == 9 && strcmp(head, "processor") == 0 &&
                           lseek64(sized, 0, SEEK_CUR) == end;
        if (sized >= 0) close(sized);
        time_t noon = 43200;
        struct tm local = {};
        bool local_time = localtime_r(&noon, &local) == &local && local.tm_year == 70 && local.tm_mon == 0;
        check(large_files && local_time, "lseek64 / pread64 / localtime_r");
        check(spin_until_other_thread_runs(spin_arm) && spin_until_other_thread_runs(spin_thumb),
              "a thread spinning on YIELD lets the thread it waits for run");
        check(signal_reaches_sleeping_thread(), "a signal reaches a thread inside usleep at once");
        check(spin_until_other_thread_runs(spin_calling),
              "a thread looping over phone functions lets the thread it waits for run");
        {
            // Unicorn (the PC's engine) has no time slicing inside game code yet.
            int (*uses_translator)(void) = (int (*)(void))dlsym(RTLD_DEFAULT, "arm32_bridge_uses_translator");
            if (uses_translator && uses_translator())
                check(spin_until_other_thread_runs(spin_plain),
                      "a thread spinning without calls or YIELD lets the thread it waits for run");
        }
        check(multiplies_arm() && multiplies_thumb(), "UMAAL / SMMUL / SMMULR / SMMLA (ARM and Thumb)");
        check(halfword_multiplies_arm() && halfword_multiplies_thumb(), "SMLA<x><y> with Q flag / SMUL<x><y> / PKHBT / PKHTB (ARM and Thumb)");
        check(neon_extras_arm() && neon_extras_thumb(), "NEON lanes, VMOVL/VSHLL, VCVT, VMAX/VMIN, immediates, permutes (ARM and Thumb)");
        check(stored_pc_offset() == 4, "STM storing PC (ARM)");
        check(raw_getpid_arm() == getpid() && raw_getpid_thumb() == getpid(), "raw svc system call (ARM and Thumb)");
        {
            unsigned arm_sum = neon_coverage_checksum(neon_coverage_arm), thumb_sum = neon_coverage_checksum(neon_coverage_thumb);
            __android_log_print(ANDROID_LOG_INFO, TAG, "neon coverage checksum %08x", arm_sum);
            for (unsigned i = 0; i < sizeof neon_coverage_out; i++)
                if (neon_coverage_out[i] != neon_coverage_previous[i]) {
                    __android_log_print(ANDROID_LOG_INFO, TAG, "neon coverage: ARM and Thumb first differ at byte %u (result %u): %02x vs %02x",
                                        i, i / 16, neon_coverage_previous[i], neon_coverage_out[i]);
                    break;
                }
            check(arm_sum == thumb_sum, "NEON coverage: ARM and Thumb give the same results");
        }
        check(neon_then_vfp_arm() == 0x3EAAAAAA && neon_then_vfp_thumb() == 0x3EAAAAAA,
              "VFP rounding mode after a NEON block loops back (ARM and Thumb)");
        check(reverse_subtract_with_carry() && push_stack_pointer(), "RSC and STM {sp, lr} with sp! (ARM)");
    }

    {
        check(dlopen_waits_for_start_up(), "two threads opening a library both wait for its start-up code");
        check(ping_pong_without_lost_wakeups(), "mutex + condition and semaphore ping-pong without lost wake-ups");
        check(futex_wake_counts(), "futex wake counts: 3 sleepers, wake 2 then 2; timed waits racing wakes");
        check(handler_reads_thread_data(), "a signal handler reads thread-specific data its thread keeps changing");
        if (bridge_flag("arm32_bridge_runs_in_parallel"))
            check(signals_reach_blocked_read(), "two signals reach a thread blocked in read() while it stays blocked");
        int gl = gl_state_follows_context();
        if (gl < 0)
            __android_log_print(ANDROID_LOG_INFO, TAG, "not runnable: GL state follows the context (no GL context here)");
        else
            check(gl == 1, "GL state follows the context: binding seen on another thread, unmap from a sharing context");
        // These have threads spin-wait without calls: Unicorn alone cannot
        // take the guest lock from a thread like that.
        if (bridge_flag("arm32_bridge_uses_translator")) {
            // every block runs twice, or every atomic runs in Unicorn: fewer rounds
            if (bridge_flag("arm32_bridge_is_checking") || bridge_flag("arm32_bridge_forces_fallbacks")) {
                kRaceAdds = 2000;
                kSmallRounds = 50;
            }
            check(shared_counters_exact(), "four threads on shared counters: atomics (32/64-bit), kuser cmpxchg, mutex, SWP lock");
            check(small_counters_exact(), "four threads on 8- and 16-bit atomics, exact per round, neighbours untouched");
            check(once_from_four_threads(), "pthread_once from four threads at once runs once");
            check(code_rewrite_across_threads(), "code rewritten while other threads run it (calls and a spinning thread)");
            check(signals_during_phone_calls(), "1000 signals to a thread making phone calls, each handled once");
            check(signal_reaches_spinning_thread(), "a signal reaches a thread busy in a loop without calls");
            check(signal_reaches_callback(), "a signal reaches a thread spinning in a qsort callback");
            if (!bridge_flag("arm32_bridge_is_checking")) { // timing means nothing when every block runs twice
                bool overlap = busy_threads_overlap();
                if (bridge_flag("arm32_bridge_runs_in_parallel")) check(overlap, "four busy threads run at the same time");
            }
        }
    }

    {
        int pair[2] = {-1, -1};
        bool made = socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0 && pair[0] > 2 && pair[1] > 2;
        char got[8] = {0};
        struct pollfd ready = {pair[1], POLLIN, 0};
        bool moved = made && send(pair[0], "ping", 4, 0) == 4 && poll(&ready, 1, 1000) == 1 &&
                     recv(pair[1], got, sizeof got, 0) == 4 && strcmp(got, "ping") == 0;
        struct timeval wait = {2, 500000}, back = {0, 0};
        socklen_t size = sizeof back;
        bool timeout = made && setsockopt(pair[1], SOL_SOCKET, SO_RCVTIMEO, &wait, sizeof wait) == 0 &&
                       getsockopt(pair[1], SOL_SOCKET, SO_RCVTIMEO, &back, &size) == 0 && back.tv_sec == 2 &&
                       back.tv_usec == 500000 && size == sizeof back;
        if (made) { close(pair[0]); close(pair[1]); }
        check(moved && timeout, "socketpair, send / poll / recv, receive timeout option");

        unsigned char binary[4] = {0};
        char text[INET_ADDRSTRLEN] = {0};
        bool addresses = inet_pton(AF_INET, "10.1.2.3", binary) == 1 && binary[0] == 10 && binary[3] == 3 &&
                         inet_ntop(AF_INET, binary, text, sizeof text) == text && strcmp(text, "10.1.2.3") == 0 &&
                         inet_addr("127.0.0.1") == htonl(0x7f000001);
        struct addrinfo hints = {}, *found = nullptr;
        hints.ai_family = AF_INET;
        hints.ai_socktype = SOCK_STREAM;
        hints.ai_flags = AI_NUMERICHOST;
        bool resolved = getaddrinfo("127.0.0.1", "80", &hints, &found) == 0 && found && found->ai_family == AF_INET &&
                        found->ai_addrlen == sizeof(struct sockaddr_in) &&
                        ((struct sockaddr_in *)found->ai_addr)->sin_port == htons(80) &&
                        ((struct sockaddr_in *)found->ai_addr)->sin_addr.s_addr == htonl(0x7f000001);
        if (found) freeaddrinfo(found);
        int tcp = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in local = {};
        local.sin_family = AF_INET;
        local.sin_addr.s_addr = htonl(0x7f000001);
        socklen_t local_size = sizeof local;
        bool listening = tcp > 2 && bind(tcp, (struct sockaddr *)&local, sizeof local) == 0 && listen(tcp, 1) == 0 &&
                         getsockname(tcp, (struct sockaddr *)&local, &local_size) == 0 && local.sin_port != 0;
        if (tcp >= 0) close(tcp);
        check(addresses && resolved && listening, "inet_pton / inet_ntop / getaddrinfo / TCP socket bind + listen");
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
