// Objective-C exceptions of the game's own classes, for the unit test
// cxx::tests::game_code_throws_and_catches (linked into throw_test).
// @catch (GBTestError*) makes the compiler write an OBJC_EHTYPE_$_GBTestError
// record that points into the runtime's objc_ehtype_vtable.

extern "C" void* objc_getClass(const char*);

__attribute__((objc_root_class))
@interface NSObject
{
  Class isa;
}
@end

@interface GBTestError : NSObject
{
@public
  int code;
}
- (int)code;
@end

@implementation GBTestError
- (int)code { return code; }
@end

@interface GBTestSubError : GBTestError
@end

@implementation GBTestSubError
@end

@interface GBTestOtherError : GBTestError
@end

@implementation GBTestOtherError
@end

// The thrown object, laid out as a GBTestError (no allocator needed).
static struct { Class isa; int code; } thrown;

__attribute__((noinline)) static void throw_sub_error(int code)
{
  thrown.isa = (Class) objc_getClass("GBTestSubError");
  thrown.code = code;
  @throw (id) &thrown;
}

// A GBTestSubError passes the GBTestOtherError clause and is caught as its
// superclass GBTestError: 31.
extern "C" int test_objc_own_class()
{
  @try
    {
      throw_sub_error(31);
    }
  @catch (GBTestOtherError* e)
    {
      return -3;
    }
  @catch (GBTestError* e)
    {
      return [e code];
    }
  @catch (id e)
    {
      return -2;
    }
  return -1;
}
