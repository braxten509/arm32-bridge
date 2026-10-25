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
extern "C" int testc_mutexes_held(); // test/testc.c

namespace {
// A destructor may join a thread that registers one of its own: the
// registration lock must not be held while destructors run.
int held_during_destructor = -1;
void note_locks(void*) { held_during_destructor = testc_mutexes_held(); }
} // namespace

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

    __cxa_atexit(note_locks, nullptr, &this_image);
    __cxa_finalize(&this_image);
    CHECK(14, held_during_destructor == 0);
}
} // namespace

// ---- GCC libstdc++'s out-of-line helpers (glue/libstdcxx.cpp), declared
// as GCC's <bits/stl_tree.h> declares them, so the names are GCC's ----
namespace std {
enum _Rb_tree_color { _S_red = false, _S_black = true };
struct _Rb_tree_node_base {
    _Rb_tree_color _M_color;
    _Rb_tree_node_base* _M_parent;
    _Rb_tree_node_base* _M_left;
    _Rb_tree_node_base* _M_right;
};
_Rb_tree_node_base* _Rb_tree_increment(_Rb_tree_node_base*);
_Rb_tree_node_base* _Rb_tree_decrement(_Rb_tree_node_base*);
void _Rb_tree_insert_and_rebalance(bool, _Rb_tree_node_base*, _Rb_tree_node_base*, _Rb_tree_node_base&);
_Rb_tree_node_base* _Rb_tree_rebalance_for_erase(_Rb_tree_node_base*, _Rb_tree_node_base&);
} // namespace std
// std::__throw_length_error(const char*): by its symbol (libc++ has its own inline one in std::__1).
extern "C" __attribute__((noreturn)) void gnu_throw_length_error(const char*) __asm__("_ZSt20__throw_length_errorPKc");

namespace {
typedef std::_Rb_tree_node_base TreeNode;
struct Keyed : TreeNode {
    int key;
};
int key(TreeNode* node) { return static_cast<Keyed*>(node)->key; }

// The red-black rules below `node` (parent links, no red node with a red
// child, the same number of black nodes on every path): that number, or -1.
int black_height(TreeNode* node, TreeNode* parent) {
    if (!node)
        return 1;
    if (node->_M_parent != parent)
        return -1;
    if (node->_M_color == std::_S_red && ((node->_M_left && node->_M_left->_M_color == std::_S_red) ||
                                           (node->_M_right && node->_M_right->_M_color == std::_S_red)))
        return -1;
    int left = black_height(node->_M_left, node), right = black_height(node->_M_right, node);
    if (left < 0 || left != right)
        return -1;
    return left + (node->_M_color == std::_S_black);
}

// The tree holds exactly the keys in `expected` (ascending), walked both
// ways, with the header's root, leftmost and rightmost right.
bool tree_is(TreeNode& header, const int* expected, int count) {
    TreeNode* root = header._M_parent;
    if (count == 0)
        return !root && header._M_left == &header && header._M_right == &header;
    if (!root || root->_M_color != std::_S_black || black_height(root, &header) < 0)
        return false;
    TreeNode* node = header._M_left;
    for (int i = 0; i < count; ++i, node = std::_Rb_tree_increment(node))
        if (node == &header || key(node) != expected[i])
            return false;
    if (node != &header)
        return false;
    for (int i = count - 1; i >= 0; --i)
        if (key(node = std::_Rb_tree_decrement(node)) != expected[i])
            return false;
    return node == header._M_left && key(header._M_right) == expected[count - 1] &&
           std::_Rb_tree_decrement(&header) == header._M_right;
}

void gnu_tree() {
    // An empty tree's header: red, no root, leftmost and rightmost itself.
    TreeNode header = {std::_S_red, nullptr, &header, &header};
    static Keyed nodes[40];
    // Inserted as std::map does: down from the root, then linked under the parent.
    for (int i = 0; i < 40; ++i) {
        Keyed* z = &nodes[i];
        z->key = (i * 17) % 40;
        TreeNode* parent = &header;
        bool left = true;
        for (TreeNode* x = header._M_parent; x; x = left ? x->_M_left : x->_M_right) {
            parent = x;
            left = z->key < key(x);
        }
        std::_Rb_tree_insert_and_rebalance(left, z, parent, header);
    }
    int all[40], odd[20];
    for (int i = 0; i < 40; ++i)
        all[i] = i;
    for (int i = 0; i < 20; ++i)
        odd[i] = 2 * i + 1;
    bool inserted = tree_is(header, all, 40);
    // Erase the even keys (the root, the leftmost, inner nodes), then the rest.
    bool erased = true;
    for (int i = 0; i < 40; ++i)
        if (nodes[i].key % 2 == 0)
            erased &= std::_Rb_tree_rebalance_for_erase(&nodes[i], header) == &nodes[i];
    erased &= tree_is(header, odd, 20);
    for (int i = 0; i < 40; ++i)
        if (nodes[i].key % 2 == 1)
            std::_Rb_tree_rebalance_for_erase(&nodes[i], header);
    CHECK(15, inserted && erased && tree_is(header, odd, 0));

    bool length = false;
    try {
        gnu_throw_length_error("too long");
    } catch (const std::length_error& e) { // libc++'s std::length_error is GCC's type
        length = std::string(e.what()) == "too long";
    } catch (...) {
    }
    CHECK(16, length);
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
    gnu_tree();
    return failed;
}
