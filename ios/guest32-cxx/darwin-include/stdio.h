#ifndef _GB_STDIO_H
#define _GB_STDIO_H
#include <sys/types.h>
#include <stdarg.h>

typedef __darwin_off_t fpos_t;
typedef struct __sFILE FILE; /* libSystem's; only pointers here */

__BEGIN_DECLS
extern FILE *__stdinp;
extern FILE *__stdoutp;
extern FILE *__stderrp;
#define stdin __stdinp
#define stdout __stdoutp
#define stderr __stderrp

#define EOF (-1)
#define BUFSIZ 1024
#define FILENAME_MAX 1024
#define FOPEN_MAX 20
#define L_tmpnam 1024
#define TMP_MAX 308915776
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define _IOFBF 0
#define _IOLBF 1
#define _IONBF 2

FILE *fopen(const char *__restrict, const char *__restrict);
FILE *freopen(const char *__restrict, const char *__restrict, FILE *__restrict);
FILE *fdopen(int, const char *);
int fclose(FILE *);
int fflush(FILE *);
size_t fread(void *__restrict, size_t, size_t, FILE *__restrict);
size_t fwrite(const void *__restrict, size_t, size_t, FILE *__restrict);
int fgetc(FILE *);
int getc(FILE *);
int getchar(void);
int ungetc(int, FILE *);
int fputc(int, FILE *);
int putc(int, FILE *);
int putchar(int);
int fputs(const char *__restrict, FILE *__restrict);
int puts(const char *);
char *fgets(char *__restrict, int, FILE *__restrict);
int fseek(FILE *, long, int);
long ftell(FILE *);
int fseeko(FILE *, off_t, int);
off_t ftello(FILE *);
void rewind(FILE *);
int fgetpos(FILE *__restrict, fpos_t *);
int fsetpos(FILE *, const fpos_t *);
void clearerr(FILE *);
int feof(FILE *);
int ferror(FILE *);
int fileno(FILE *);
void perror(const char *);
int remove(const char *);
int rename(const char *, const char *);
FILE *tmpfile(void);
char *tmpnam(char *);
void setbuf(FILE *__restrict, char *__restrict);
int setvbuf(FILE *__restrict, char *__restrict, int, size_t);
int printf(const char *__restrict, ...);
int fprintf(FILE *__restrict, const char *__restrict, ...);
int sprintf(char *__restrict, const char *__restrict, ...);
int snprintf(char *__restrict, size_t, const char *__restrict, ...);
int vprintf(const char *__restrict, va_list);
int vfprintf(FILE *__restrict, const char *__restrict, va_list);
int vsprintf(char *__restrict, const char *__restrict, va_list);
int vsnprintf(char *__restrict, size_t, const char *__restrict, va_list);
int asprintf(char **__restrict, const char *__restrict, ...);
int vasprintf(char **__restrict, const char *__restrict, va_list);
int scanf(const char *__restrict, ...);
int fscanf(FILE *__restrict, const char *__restrict, ...);
int sscanf(const char *__restrict, const char *__restrict, ...);
int vscanf(const char *__restrict, va_list);
int vfscanf(FILE *__restrict, const char *__restrict, va_list);
int vsscanf(const char *__restrict, const char *__restrict, va_list);
__END_DECLS
#endif
