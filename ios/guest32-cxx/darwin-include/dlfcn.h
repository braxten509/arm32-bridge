#ifndef _GB_DLFCN_H
#define _GB_DLFCN_H
#include <_darwin.h>

#define RTLD_LAZY 0x1
#define RTLD_NOW 0x2
#define RTLD_LOCAL 0x4
#define RTLD_GLOBAL 0x8
#define RTLD_NOLOAD 0x10
#define RTLD_NODELETE 0x80
#define RTLD_DEFAULT ((void *)-2)

__BEGIN_DECLS
void *dlopen(const char *, int);
void *dlsym(void *__restrict, const char *__restrict);
int dlclose(void *);
char *dlerror(void);
__END_DECLS
#endif
