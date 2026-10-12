#include <stdio.h>
#include <stdlib.h>
#include <time.h>
int bench(int rounds);
int main(int argc, char **argv) {
    int rounds = argc > 1 ? atoi(argv[1]) : 1000;
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    int result = bench(rounds);
    clock_gettime(CLOCK_MONOTONIC, &end);
    double ms = (end.tv_sec - start.tv_sec) * 1e3 + (end.tv_nsec - start.tv_nsec) / 1e6;
    printf("bench returned %d (%#x)\nnative 64-bit time: %.1f ms\n", result, (unsigned)result, ms);
    return 0;
}
