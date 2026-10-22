// Game Bridge: Objective-C exceptions on top of the C++ ones, as Apple's
// objc4 does it (objc-exception.mm): an Objective-C exception is a C++
// exception whose thrown object holds the `id` and whose type_info is an
// Objective-C one; @catch clauses name OBJC_EHTYPE_* records; the
// Objective-C personality routine is the C++ one.

#include <typeinfo>
#include <exception>
#include <new>
#include <cstddef>
#include <unwind.h>
#include "unwind-cxx.h"

using namespace __cxxabiv1;

extern "C" {
  // Answers from Game Bridge's Objective-C runtime.
  int gamebridge_objc_is_kind_of(void* object, const char* class_name);
  const char* gamebridge_objc_class_name(void* object);
  void gamebridge_objc_uncaught(void* object);
}

namespace
{
  // objc4's struct objc_typeinfo: { vtable, name, cls_unremapped }.
  class objc_ehtype : public std::type_info
  {
  public:
    explicit objc_ehtype(const char* name) : std::type_info(name), cls(0) { }
    virtual ~objc_ehtype();
    virtual bool __do_catch(const std::type_info* thrown, void** object, unsigned outer) const;
    void* cls;
  };

  objc_ehtype::~objc_ehtype() { }

  bool is_objc(const std::type_info* type)
  { return dynamic_cast<const objc_ehtype*>(type) != 0; }

  // The thrown object of an Objective-C exception.
  struct objc_exception
  {
    void* object;
    objc_ehtype type;
  };

  // A @catch (id) clause takes every Objective-C exception, a
  // @catch (Class*) one those of that class or a subclass. C++ exceptions
  // never match.
  bool
  objc_ehtype::__do_catch(const std::type_info* thrown, void** object, unsigned) const
  {
    if (!is_objc(thrown))
      return false;
    void* exception = *static_cast<void**>(*object);
    if (__builtin_strcmp(name(), "id") != 0
	&& !gamebridge_objc_is_kind_of(exception, name()))
      return false;
    *object = exception;
    return true;
  }

  void objc_exception_destructor(void*) { }   // objects are never freed here
}

extern "C" {

objc_ehtype gamebridge_ehtype_id __asm__("OBJC_EHTYPE_id")("id");
objc_ehtype gamebridge_ehtype_nsexception __asm__("OBJC_EHTYPE_$_NSException")("NSException");

void __attribute__((noreturn))
objc_exception_throw(void* object)
{
  objc_exception* exception = static_cast<objc_exception*>(
      __cxa_allocate_exception(sizeof(objc_exception)));
  exception->object = object;
  new (&exception->type) objc_ehtype(gamebridge_objc_class_name(object));
  __cxa_throw(exception, &exception->type, objc_exception_destructor);
}

void __attribute__((noreturn))
objc_exception_rethrow()
{
  __cxa_rethrow();
}

void*
objc_begin_catch(void* exception)
{
  return __cxa_begin_catch(exception);
}

void
objc_end_catch()
{
  __cxa_end_catch();
}

void __attribute__((noreturn))
objc_terminate()
{
  std::terminate();
}

_Unwind_Reason_Code
__objc_personality_v0(int version, _Unwind_Action actions,
		      _Unwind_Exception_Class exception_class,
		      struct _Unwind_Exception* exception,
		      struct _Unwind_Context* context)
{
  return __gxx_personality_v0(version, actions, exception_class, exception, context);
}

// For std::terminate's report: the `id` of the Objective-C exception being
// handled, or null.
void*
gamebridge_objc_current_exception()
{
  __cxa_eh_globals* globals = __cxa_get_globals();
  __cxa_exception* header = globals->caughtExceptions;
  if (header == 0 || !is_objc(header->exceptionType))
    return 0;
  return static_cast<objc_exception*>(static_cast<void*>(header + 1))->object;
}

}
