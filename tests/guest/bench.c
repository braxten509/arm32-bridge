/* Speed test: the same source is built as 32-bit ARM (run through the
 * bridge) and as native 64-bit (run directly), so the two times compare. */
static int table[1024];

int bench(int rounds) {
    unsigned hash = 2166136261u;
    float sum = 0.0f;
    for (int round = 0; round < rounds; round++) {
        for (int i = 0; i < 1024; i++) {
            hash = (hash ^ (unsigned)(i + round)) * 16777619u;
            table[i] = (table[i] + (int)(hash >> 7)) & 0xFFFF;
            sum += (float)(table[i] & 255) * 0.5f;
            if (sum > 1.0e6f) sum *= 0.25f;
        }
    }
    return (int)hash ^ (int)sum;
}
