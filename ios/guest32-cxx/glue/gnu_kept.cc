// GNU libstdc++'s inline functions that Apple's libstdc++.6.dylib exported
// (it was built at -O0 with inline functions kept, so every one it used
// stayed a function) and that games built at low optimization import. The
// rest of Apple's armv7 export list (exports/libstdcxx-armv7.exp) comes from
// the GNU sources as they are; these are what that build kept beyond them.
//
// Not optimized, so the inline functions they call (ctype<char>'s private
// _M_widen_init) stay functions too, as in Apple's build.
#pragma clang optimize off

#include <bits/boost_concept_check.h>
#include <ext/bitmap_allocator.h>
#include <iterator>
#include <locale>
#include <string>

namespace {
// Using a member function's address emits it; it has default visibility.
__attribute__((used)) char (std::ctype<char>::*const widen_char)(char) const = &std::ctype<char>::widen;
__attribute__((used)) const char* (std::ctype<char>::*const widen_range)(const char*, const char*, char*) const =
    &std::ctype<char>::widen;
__attribute__((used)) char (std::ctype<char>::*const narrow_char)(char, char) const = &std::ctype<char>::narrow;
} // namespace

template std::istreambuf_iterator<char>::int_type std::istreambuf_iterator<char>::_M_get() const;
template std::istreambuf_iterator<wchar_t>::int_type std::istreambuf_iterator<wchar_t>::_M_get() const;

template __gnu_cxx::__enable_if<std::__is_char<char>::__value, int>::__type
std::num_get<char>::_M_find<char>(const char*, size_t, char) const;
template __gnu_cxx::__enable_if<std::__is_char<wchar_t>::__value, int>::__type
std::num_get<wchar_t>::_M_find<wchar_t>(const wchar_t*, size_t, wchar_t) const;
// Apple's name for them: this clang writes the std::__is_char<_CharT2>::__value
// in the return type as "sr9__is_charIT_EE" (today's Itanium rule), Apple's
// compiler as "srSt9__is_charIT_E"; the same functions under both names.
__asm__(".globl _ZNKSt7num_getIcSt19istreambuf_iteratorIcSt11char_traitsIcEEE7_M_findIcEEN9__gnu_cxx11__enable_ifIXsrSt9__is_charIT_E7__valueEiE6__typeEPKS9_mS9_\n"
        ".thumb_set _ZNKSt7num_getIcSt19istreambuf_iteratorIcSt11char_traitsIcEEE7_M_findIcEEN9__gnu_cxx11__enable_ifIXsrSt9__is_charIT_E7__valueEiE6__typeEPKS9_mS9_, "
        "_ZNKSt7num_getIcSt19istreambuf_iteratorIcSt11char_traitsIcEEE7_M_findIcEEN9__gnu_cxx11__enable_ifIXsr9__is_charIT_EE7__valueEiE6__typeEPKS8_mS8_\n"
        ".globl _ZNKSt7num_getIwSt19istreambuf_iteratorIwSt11char_traitsIwEEE7_M_findIwEEN9__gnu_cxx11__enable_ifIXsrSt9__is_charIT_E7__valueEiE6__typeEPKS9_mS9_\n"
        ".thumb_set _ZNKSt7num_getIwSt19istreambuf_iteratorIwSt11char_traitsIwEEE7_M_findIwEEN9__gnu_cxx11__enable_ifIXsrSt9__is_charIT_E7__valueEiE6__typeEPKS9_mS9_, "
        "_ZNKSt7num_getIwSt19istreambuf_iteratorIwSt11char_traitsIwEEE7_M_findIwEEN9__gnu_cxx11__enable_ifIXsr9__is_charIT_EE7__valueEiE6__typeEPKS8_mS8_\n");

template bool __gnu_cxx::__detail::_Ffit_finder<__gnu_cxx::bitmap_allocator<char>::_Alloc_block*>::operator()(
    std::pair<__gnu_cxx::bitmap_allocator<char>::_Alloc_block*, __gnu_cxx::bitmap_allocator<char>::_Alloc_block*>);
template bool __gnu_cxx::__detail::_Ffit_finder<__gnu_cxx::bitmap_allocator<wchar_t>::_Alloc_block*>::operator()(
    std::pair<__gnu_cxx::bitmap_allocator<wchar_t>::_Alloc_block*, __gnu_cxx::bitmap_allocator<wchar_t>::_Alloc_block*>);

// The concept checks GCC's headers run (empty when the code compiles).
template void __gnu_cxx::__function_requires<__gnu_cxx::_OutputIteratorConcept<std::ostreambuf_iterator<char>, char> >();
template void __gnu_cxx::__function_requires<__gnu_cxx::_OutputIteratorConcept<std::ostreambuf_iterator<wchar_t>, wchar_t> >();
template void __gnu_cxx::__function_requires<
    __gnu_cxx::_RandomAccessIteratorConcept<__gnu_cxx::__normal_iterator<char*, std::string> > >();
template void __gnu_cxx::__function_requires<
    __gnu_cxx::_RandomAccessIteratorConcept<__gnu_cxx::__normal_iterator<const char*, std::string> > >();
template void __gnu_cxx::__function_requires<
    __gnu_cxx::_RandomAccessIteratorConcept<__gnu_cxx::__normal_iterator<wchar_t*, std::wstring> > >();
template void __gnu_cxx::__function_requires<
    __gnu_cxx::_RandomAccessIteratorConcept<__gnu_cxx::__normal_iterator<const wchar_t*, std::wstring> > >();
