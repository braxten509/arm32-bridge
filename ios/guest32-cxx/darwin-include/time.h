#ifndef _GB_TIME_H
#define _GB_TIME_H
#include <sys/types.h>

struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
    long tm_gmtoff;
    char *tm_zone;
};
struct timespec {
    __darwin_time_t tv_sec;
    long tv_nsec;
};

#define CLOCKS_PER_SEC 1000000
#define TIME_UTC 1
typedef enum {
    _CLOCK_REALTIME = 0,
    _CLOCK_MONOTONIC = 6,
    _CLOCK_MONOTONIC_RAW = 4,
    _CLOCK_MONOTONIC_RAW_APPROX = 5,
    _CLOCK_UPTIME_RAW = 8,
    _CLOCK_UPTIME_RAW_APPROX = 9,
    _CLOCK_PROCESS_CPUTIME_ID = 12,
    _CLOCK_THREAD_CPUTIME_ID = 16
} clockid_t;
#define CLOCK_REALTIME _CLOCK_REALTIME
#define CLOCK_MONOTONIC _CLOCK_MONOTONIC
#define CLOCK_MONOTONIC_RAW _CLOCK_MONOTONIC_RAW
#define CLOCK_UPTIME_RAW _CLOCK_UPTIME_RAW
#define CLOCK_PROCESS_CPUTIME_ID _CLOCK_PROCESS_CPUTIME_ID
#define CLOCK_THREAD_CPUTIME_ID _CLOCK_THREAD_CPUTIME_ID

__BEGIN_DECLS
clock_t clock(void);
double difftime(time_t, time_t);
time_t mktime(struct tm *);
time_t time(time_t *);
char *asctime(const struct tm *);
char *ctime(const time_t *);
struct tm *gmtime(const time_t *);
struct tm *localtime(const time_t *);
struct tm *gmtime_r(const time_t *__restrict, struct tm *__restrict);
struct tm *localtime_r(const time_t *__restrict, struct tm *__restrict);
size_t strftime(char *__restrict, size_t, const char *__restrict, const struct tm *__restrict);
size_t strftime_l(char *__restrict, size_t, const char *__restrict, const struct tm *__restrict, locale_t);
char *strptime(const char *__restrict, const char *__restrict, struct tm *__restrict);
int nanosleep(const struct timespec *, struct timespec *);
int clock_gettime(clockid_t, struct timespec *);
__uint64_t clock_gettime_nsec_np(clockid_t);
int timespec_get(struct timespec *, int);
__END_DECLS
#endif
