// The libc++ side of test/gnutest.cc: an exception thrown by GNU libstdc++
// code (built from a copy-on-write string) caught by libc++ code as its
// std::exception, as a game mixing both libraries would.

#include <cstring>
#include <exception>

extern "C" __attribute__((visibility("default"))) int gnucatch_what_is(void (*thrower)(), const char* expected) {
    try {
        thrower();
    } catch (const std::exception& e) {
        return std::strcmp(e.what(), expected) == 0;
    } catch (...) {
    }
    return 0;
}
