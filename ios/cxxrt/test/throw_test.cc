// A small iPhone (arm64 Mach-O) program that throws and catches C++
// exceptions, for the unit test cxx::tests::game_code_throws_and_catches.
// Built by ios/tools/build-cxx-test.sh (no Apple SDK needed: the few
// library declarations are written out here).

namespace std
{
  class exception
  {
  public:
    exception() throw() { }
    virtual ~exception() throw();
    virtual const char* what() const throw();
  };
  void __throw_out_of_range(const char*) __attribute__((noreturn));
  bool uncaught_exception() throw();
}

extern "C" int strcmp(const char*, const char*);

struct Problem
{
  int code;
  Problem(int c) : code(c) { }
  virtual ~Problem() { }
};

struct BigProblem : Problem
{
  BigProblem(int c) : Problem(c) { }
};

static int cleanups;

struct Cleanup
{
  ~Cleanup() { cleanups++; }
};

__attribute__((noinline)) static void deepest(int code)
{
  Cleanup c;
  if (code)
    throw BigProblem(code);
}

__attribute__((noinline)) static int middle(int code)
{
  Cleanup c;
  double kept = code * 1.5;      // lives in a callee-saved register
  deepest(code);
  return (int) kept;
}

// A derived exception caught by base class two frames up, with the
// cleanups on the way run: 7 + 100 * 2.
extern "C" int test_catch_by_base()
{
  cleanups = 0;
  try
    {
      middle(7);
    }
  catch (Problem& p)
    {
      return p.code + 100 * cleanups;
    }
  return -1;
}

// Rethrow from a catch (...) into an outer handler: 9 + 10.
extern "C" int test_rethrow()
{
  int seen = 0;
  try
    {
      try
	{
	  deepest(9);
	}
      catch (...)
	{
	  seen = 10;
	  throw;
	}
    }
  catch (BigProblem& p)
    {
      return p.code + seen;
    }
  return -1;
}

// An exception thrown inside the C++ runtime (not the game) and caught
// by the game as std::exception, its what() read back: 1 on success.
extern "C" int test_runtime_throw()
{
  try
    {
      std::__throw_out_of_range("gone");
    }
  catch (std::exception& e)
    {
      return strcmp(e.what(), "gone") == 0;
    }
  return -1;
}

// No throw: the handler isn't entered and cleanups still run once: 1.
extern "C" int test_no_throw()
{
  cleanups = 0;
  try
    {
      middle(0);
    }
  catch (...)
    {
      return -1;
    }
  return cleanups == 2 ? 1 : -2;
}

extern "C" __attribute__((noinline)) void throw_big_problem(int code)
{
  throw BigProblem(code);
}

// A frame whose unwind info uses DW_CFA_remember_state/restore_state (so
// the linker keeps a DWARF FDE for it): the early return comes first and
// changes the CFA; the throwing call after it is back on the remembered
// CFA (sp + 32). Returns 0 when code is 0, else throws BigProblem(code).
// Before it, a frame that is never unwound, whose FDE build-cxx-test.sh
// breaks after linking (its CIE pointer points far outside __eh_frame):
// the unwinder must skip that record and still find remembered_frame's.
extern "C" int remembered_frame(int code);
__asm__(".text\n"
	".p2align 2\n"
	"_broken_fde_frame:\n"
	".cfi_startproc\n"
	".cfi_remember_state\n"
	".cfi_restore_state\n"
	"ret\n"
	".cfi_endproc\n"
	".p2align 2\n"
	"_remembered_frame:\n"
	".cfi_startproc\n"
	"stp x29, x30, [sp, #-32]!\n"
	".cfi_def_cfa_offset 32\n"
	".cfi_offset w30, -24\n"
	".cfi_offset w29, -32\n"
	"mov x29, sp\n"
	"cbnz w0, 1f\n"
	".cfi_remember_state\n"
	"ldp x29, x30, [sp], #32\n"
	".cfi_def_cfa_offset 0\n"
	".cfi_restore w30\n"
	".cfi_restore w29\n"
	"ret\n"
	"1:\n"
	".cfi_restore_state\n"
	"bl _throw_big_problem\n"
	"ldp x29, x30, [sp], #32\n"
	"ret\n"
	".cfi_endproc\n");

// Caught through the remembered frame: 41.
extern "C" int test_remembered_state()
{
  try
    {
      remembered_frame(41);
    }
  catch (BigProblem& p)
    {
      return p.code + (remembered_frame(0) == 0 ? 0 : 1000);
    }
  return -1;
}

struct Explodes
{
  Explodes() { throw 5; }
};

// The exception object's constructor throws: the first exception is
// freed, never thrown, so afterwards nothing is uncaught: 1.
extern "C" int test_failed_throw_not_uncaught()
{
  try
    {
      throw Explodes();
    }
  catch (int)
    {
    }
  return std::uncaught_exception() ? 0 : 1;
}

int main()
{
  return 0;
}
