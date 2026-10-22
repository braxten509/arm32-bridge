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

int main()
{
  return 0;
}
