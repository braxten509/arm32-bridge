// GCC libstdc++'s exception constructors from a std::string, for the GNU
// half of the library (README "GNU libstdc++"): GCC's <stdexcept> builds
// std::logic_error and the rest from its copy-on-write string, while the
// classes themselves (vtables, type_info, destructors, what()) are
// libc++abi's, one definition each, which GCC code catches and throws as its own.
//
// Both layouts are { vtable, pointer to the characters }, with
// { length, capacity, refcount } before the characters: libc++'s
// __libcpp_refstring was made to match GCC's string, so GCC code that copies
// or destroys the message in place (inlined copy constructors) still works.
// The constructors here copy the text into a new libc++ message.

#include <cstdio>
#include <cstdlib>
#include <cxxabi.h>
#include <new>
#include <stdexcept>
#include <typeinfo>

#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {
// GCC's std::string: one pointer to its characters (always 0-terminated).
struct GnuString {
    const char* characters;
};
} // namespace

// Class(const std::string&), complete (C1) and base (C2) object constructors;
// with no virtual bases both build the same object. "Ss" is GCC's std::string.
#define FROM_GNU_STRING(Class, mangled)                                                                  \
    EXPORT void gnu_##Class##_complete(void* self, const GnuString& what) __asm__(mangled "C1ERKSs"); \
    EXPORT void gnu_##Class##_base(void* self, const GnuString& what) __asm__(mangled "C2ERKSs");     \
    void gnu_##Class##_complete(void* self, const GnuString& what) { new (self) std::Class(what.characters); } \
    void gnu_##Class##_base(void* self, const GnuString& what) { new (self) std::Class(what.characters); }

FROM_GNU_STRING(logic_error, "_ZNSt11logic_error")
FROM_GNU_STRING(domain_error, "_ZNSt12domain_error")
FROM_GNU_STRING(invalid_argument, "_ZNSt16invalid_argument")
FROM_GNU_STRING(length_error, "_ZNSt12length_error")
FROM_GNU_STRING(out_of_range, "_ZNSt12out_of_range")
FROM_GNU_STRING(runtime_error, "_ZNSt13runtime_error")
FROM_GNU_STRING(range_error, "_ZNSt11range_error")
FROM_GNU_STRING(overflow_error, "_ZNSt14overflow_error")
FROM_GNU_STRING(underflow_error, "_ZNSt15underflow_error")

// __gnu_cxx::__verbose_terminate_handler(): GCC's terminate handler, which
// games pass to std::set_terminate: names the exception that is in flight
// and its what() on stderr, then aborts (as GCC's vterminate.cc).
namespace __gnu_cxx {
__attribute__((visibility("default"), noreturn)) void __verbose_terminate_handler();
}
void __gnu_cxx::__verbose_terminate_handler() {
    static bool terminating;
    if (terminating) {
        std::fputs("terminate called recursively\n", stderr);
        std::abort();
    }
    terminating = true;
    if (const std::type_info* type = abi::__cxa_current_exception_type()) {
        int status = -1;
        char* readable = abi::__cxa_demangle(type->name(), nullptr, nullptr, &status);
        std::fputs("terminate called after throwing an instance of '", stderr);
        std::fputs(status == 0 ? readable : type->name(), stderr);
        std::fputs("'\n", stderr);
        try {
            throw;
        } catch (const std::exception& e) {
            std::fputs("  what():  ", stderr);
            std::fputs(e.what(), stderr);
            std::fputs("\n", stderr);
        } catch (...) {
        }
    } else {
        std::fputs("terminate called without an active exception\n", stderr);
    }
    std::abort();
}
