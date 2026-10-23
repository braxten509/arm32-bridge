// Checks libguest32cxx.so the way 32-bit iPhone game code uses it. Built
// with the library's own compiler settings (Apple armv7 ABI, SjLj
// exceptions) into an ELF library; `run_all` returns 0 when every check
// passes, otherwise a bit per failed check (see the CHECK numbers).
//
// What can't be checked by compiling against the same headers is checked
// against what TownCraft's own inlined code does (see ../README.md):
// the string bytes, and 64-bit arguments in packed register pairs.

#include <stdexcept>
#include <string>
#include <sstream>
#include <typeinfo>

namespace {

unsigned failed;
#define CHECK(bit, condition) \
    do {                      \
        if (!(condition))     \
            failed |= 1u << (bit); \
    } while (0)

// ---- strings: libc++'s usual layout, as TownCraft reads it ----
// short: byte 0 = size << 1 (bit 0 clear), characters from byte 1;
// long: word 0 = capacity | 1, word 1 = size, word 2 = data.
void strings() {
    std::string small("abc", 3); // basic_string::__init, in the library
    const unsigned char* bytes = reinterpret_cast<const unsigned char*>(&small);
    CHECK(0, sizeof(std::string) == 12 && bytes[0] == 3 << 1 && bytes[1] == 'a' && bytes[3] == 'c' && bytes[4] == 0);

    const char* text = "a string long enough to live on the heap";
    std::string large(text);
    const unsigned* words = reinterpret_cast<const unsigned*>(&large);
    CHECK(1, (words[0] & 1) == 1 && (words[0] & ~1u) > 40 && words[1] == 40 &&
                 words[2] == reinterpret_cast<unsigned>(large.data()) && large.compare(text) == 0);

    large.append(large); // out-of-line growth in the library
    large.erase(10, 70);
    CHECK(2, large.size() == 10 && large == "a string l" && large.find('g') == 7);
}

// ---- exceptions across SjLj frames ----
int destroyed;
struct Cleanup {
    ~Cleanup() { ++destroyed; }
};

struct Base {
    virtual ~Base() {}
    int code = 0;
};
struct Derived : Base {
    explicit Derived(int value) { code = value; }
};

__attribute__((noinline)) void throws_derived(int value) {
    Cleanup cleanup;
    throw Derived(value);
}

__attribute__((noinline)) char out_of_range_inside_library(const std::string& s) {
    Cleanup cleanup;   // a frame with a cleanup between the throw and the catch
    return s.at(100);  // basic_string::at throws from the library
}

__attribute__((noinline)) void rethrows() {
    try {
        throws_derived(7);
    } catch (...) {
        throw;
    }
}

void exceptions() {
    destroyed = 0;
    bool caught = false;
    try {
        out_of_range_inside_library("short");
    } catch (const std::out_of_range& e) {
        caught = e.what() != nullptr && e.what()[0] != 0;
    } catch (...) {
    }
    CHECK(3, caught && destroyed == 1);

    int code = 0;
    try {
        throws_derived(5);
    } catch (const Base& e) { // caught by base class: __si_class_type_info
        code = e.code;
    }
    CHECK(4, code == 5 && destroyed == 2);

    code = 0;
    try {
        rethrows();
    } catch (Derived& e) {
        code = e.code;
    }
    CHECK(5, code == 7 && destroyed == 3);

    bool logic = false;
    try {
        throw std::invalid_argument("bad");
    } catch (const std::logic_error& e) {
        logic = std::string(e.what()) == "bad";
    }
    CHECK(6, logic);
}

// ---- guards, dynamic_cast, type_info ----
int constructed;
struct Once {
    Once() { ++constructed; }
};
Once& once() {
    static Once value; // __cxa_guard_acquire / release
    return value;
}

void runtime_support() {
    once();
    once();
    CHECK(7, constructed == 1);

    Derived derived(3);
    Base* base = &derived;
    CHECK(8, dynamic_cast<Derived*>(base) == &derived && typeid(*base) == typeid(Derived));
    Base plain;
    CHECK(9, dynamic_cast<Derived*>(&plain) == nullptr);
}

// ---- streams ----
void streams() {
    std::ostringstream out;
    out << 42 << ' ' << -7 << "x";
    CHECK(10, out.str() == "42 -7x");
    std::istringstream in("12 34");
    int a = 0, b = 0;
    in >> a >> b;
    CHECK(11, a == 12 && b == 34);
}

// ---- 64-bit arguments: Apple armv7 passes them in the next two registers
// (no even pair), as TownCraft's call to operator<<(double) shows. Called
// by hand so the check doesn't depend on this file's compiler settings.
void packed_pairs() {
    alignas(4) unsigned char result[sizeof(std::string)] = {};
    std::string* text = reinterpret_cast<std::string*>(result);
    // std::to_string(long long): result pointer in r0, the value in r1:r2.
    register unsigned r0 asm("r0") = reinterpret_cast<unsigned>(result);
    register unsigned r1 asm("r1") = 1234567;      // low word
    register unsigned r2 asm("r2") = 1;            // high word: 2^32 + 1234567 = 4296201863
    register unsigned r3 asm("r3") = 0xDEAD;       // AAPCS would read r2:r3 instead
    asm volatile("bl _ZNSt3__19to_stringEx"
                 : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3)
                 :
                 : "r12", "lr", "memory", "cc", "q0", "q1", "q2", "q3", "q8", "q9", "q10", "q11", "q12", "q13", "q14", "q15");
    CHECK(12, *text == "4296201863");
    text->~basic_string();
}

// ---- __cxa_atexit / __cxa_finalize (Apple's are in libSystem; games import them) ----
int finalized;
void count_finalize(void* by) { finalized += *static_cast<int*>(by); }

} // namespace

extern "C" int __cxa_atexit(void (*)(void*), void*, void*);
extern "C" void __cxa_finalize(void*);

namespace {
void at_exit() {
    static int one = 1, ten = 10;
    static char this_image, other_image;
    finalized = 0;
    __cxa_atexit(count_finalize, &one, &this_image);
    __cxa_atexit(count_finalize, &ten, &other_image);
    __cxa_finalize(&this_image);
    __cxa_finalize(&this_image); // each handler runs once
    CHECK(13, finalized == 1);
    __cxa_finalize(&other_image);
    CHECK(13, finalized == 11);
}
} // namespace

extern "C" __attribute__((visibility("default"))) unsigned run_all() {
    failed = 0;
    strings();
    exceptions();
    runtime_support();
    streams();
    packed_pairs();
    at_exit();
    return failed;
}
