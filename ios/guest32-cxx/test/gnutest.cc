// Checks the GNU half of libguest32cxx.so (Apple's GCC 4.2.1 libstdc++) the
// way a GCC-era iPhone game uses it: compiled against the same GNU headers,
// so its inlined code reads the copy-on-write string, the streams and the
// locale directly, as a game's does. `gnu_run_all` returns 0 when every
// check passes, otherwise a bit per failed check (see the CHECK numbers).

#include <cstdio>
#include <cstring>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <list>
#include <locale>
#include <map>
#include <sstream>
#include <stdexcept>
#include <string>
#include <unistd.h>

// test/gnucatch.cpp (libc++'s headers): 1 when `thrower` throws something
// caught there as std::exception whose what() is `expected`.
extern "C" int gnucatch_what_is(void (*thrower)(), const char* expected);

namespace {

unsigned failed;
#define CHECK(bit, condition) \
    do {                      \
        if (!(condition))     \
            failed |= 1u << (bit); \
    } while (0)

// ---- the copy-on-write string: { length, capacity, refcount } right
// before the characters; refcount 0 = one owner, -1 = unshareable ----
struct Rep {
    size_t length, capacity;
    int refcount;
};
const Rep& rep(const std::string& s) { return reinterpret_cast<const Rep*>(s.data())[-1]; }

// Every empty string's characters: the library's _S_empty_rep_storage, after its Rep.
extern "C" size_t empty_rep_storage[] __asm__("_ZNSs4_Rep20_S_empty_rep_storageE");

void strings() {
    std::string a("hello");
    std::string b(a); // shares a's characters
    CHECK(0, a.data() == b.data() && rep(a).refcount == 1 && rep(a).length == 5 && rep(a).capacity >= 5);
    b[0] = 'j'; // writable access: b gets its own copy, which is no longer shared
    std::string c(b);
    CHECK(1, a.data() != b.data() && a == "hello" && b == "jello" && rep(a).refcount == 0 &&
                 rep(b).refcount == -1 && c.data() != b.data() && c == "jello");

    std::string e1, e2;
    CHECK(2, e1.data() == e2.data() && e1.data() == reinterpret_cast<const char*>(empty_rep_storage + 3) &&
                 e1.size() == 0 && *e1.c_str() == 0);

    std::string s("one two three");
    s.append(" four").append(2, '!');
    s.replace(4, 3, "2");
    bool edited = s == "one 2 three four!!" && s.substr(6, 5) == "three" && s.find("four") == 12 &&
                  s.find('x') == std::string::npos && s.rfind('e') == 10 && s.find_first_of("tf") == 6 &&
                  s.compare("one") > 0;
    s.insert(0, ">> ");
    s.erase(0, 3);
    s.resize(5);
    std::string grown;
    for (int i = 0; i < 1000; ++i)
        grown.push_back(static_cast<char>('a' + i % 26));
    CHECK(3, edited && s == "one 2" && grown.size() == 1000 && grown[999] == 'a' + 999 % 26 &&
                 rep(grown).capacity >= 1000);
}

// ---- string streams ----
void streams() {
    std::ostringstream out;
    out << 42 << ' ' << 2.5 << ' ' << std::string("x") << ' ' << std::setw(4) << std::setfill('0') << 7 << ' '
        << -3LL << ' ' << 1.0f / 3;
    CHECK(4, out.str() == "42 2.5 x 0007 -3 0.333333");

    std::istringstream in("12 3.5 word -7");
    int i = 0;
    float f = 0;
    std::string word;
    long l = 0;
    in >> i >> f >> word >> l;
    bool read = i == 12 && f == 3.5f && word == "word" && l == -7 && !in.fail();
    in >> i;
    CHECK(5, read && in.fail() && in.eof());

    std::istringstream lines("first line\nsecond\n\nlast");
    std::string line[4];
    bool got = true;
    for (int n = 0; n < 4; ++n)
        got &= static_cast<bool>(std::getline(lines, line[n]));
    CHECK(6, got && line[0] == "first line" && line[1] == "second" && line[2].empty() && line[3] == "last" &&
                 !std::getline(lines, line[0]));
}

// ---- files: written, read back, then read and written through one fstream ----
void files() {
    char path[64];
    std::snprintf(path, sizeof path, "/tmp/gamebridge-gnutest-%d.txt", static_cast<int>(getpid()));
    {
        std::ofstream out(path);
        out << "alpha 12\nbeta gamma\n";
    }
    std::ifstream in(path);
    std::string word, rest;
    int number = 0;
    in >> word >> number;
    in.ignore();
    std::getline(in, rest);
    bool read = in.is_open() && word == "alpha" && number == 12 && rest == "beta gamma";
    in.close();

    std::fstream both(path, std::ios::in | std::ios::out);
    both.seekp(6);
    both << "34";
    both.seekg(0);
    both >> word >> number;
    bool updated = both.good() && word == "alpha" && number == 34;
    both.close();
    CHECK(7, read && updated && std::remove(path) == 0 && !std::ifstream(path).is_open());
}

// ---- locale ----
void locales() {
    const std::ctype<char>& ctype = std::use_facet<std::ctype<char> >(std::locale::classic());
    CHECK(8, ctype.widen('a') == 'a' && ctype.toupper('q') == 'Q' && ctype.is(std::ctype_base::digit, '7') &&
                 !ctype.is(std::ctype_base::space, 'x') && std::locale() == std::locale::classic() &&
                 std::locale::classic().name() == "C" && std::has_facet<std::numpunct<char> >(std::locale()));
}

// ---- exceptions from COW strings ----
void throw_runtime_error() { throw std::runtime_error(std::string("bad thing")); }
void throw_out_of_range() { std::string("abc").at(10); }

void exceptions() {
    CHECK(9, gnucatch_what_is(throw_runtime_error, "bad thing") && gnucatch_what_is(throw_out_of_range, "basic_string::at"));
    bool caught = false;
    try {
        throw_out_of_range();
    } catch (const std::logic_error& e) {
        caught = std::strcmp(e.what(), "basic_string::at") == 0;
    }
    CHECK(10, caught);
}

// ---- the standard streams (ios_base::Init ran from <iostream>), list and map ----
void containers() {
    CHECK(11, std::cout.rdbuf() != 0 && std::cout.good() && (std::cout.flags() & std::ios_base::dec) &&
                  std::cin.tie() == &std::cout && std::cerr.rdbuf() != 0);

    std::list<int> numbers;
    for (int i = 0; i < 6; ++i)
        numbers.push_back((i * 5) % 6); // 0 5 4 3 2 1
    numbers.reverse();                  // 1 2 3 4 5 0
    numbers.sort();                     // 0 1 2 3 4 5
    std::list<int> other(2, 9);
    numbers.splice(numbers.begin(), other);
    std::list<int>::const_iterator it = numbers.begin();
    bool listed = numbers.size() == 8 && *it++ == 9 && *it++ == 9 && *it == 0 && numbers.back() == 5 && other.empty();

    std::map<std::string, int> names;
    names["zebra"] = 1;
    names["apple"] = 2;
    names["mango"] = 3;
    names.erase("apple");
    std::string order;
    for (std::map<std::string, int>::const_iterator n = names.begin(); n != names.end(); ++n)
        order += n->first + ",";
    CHECK(12, listed && order == "mango,zebra," && names["zebra"] == 1);
}

} // namespace

extern "C" __attribute__((visibility("default"))) unsigned gnu_run_all() {
    failed = 0;
    strings();
    streams();
    files();
    locales();
    exceptions();
    containers();
    return failed;
}
