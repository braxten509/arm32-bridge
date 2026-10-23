#ifndef _GB_NL_TYPES_H
#define _GB_NL_TYPES_H
#include <_darwin.h>

#define NL_SETD 1
#define NL_CAT_LOCALE 1
typedef struct __nl_cat_d *nl_catd;
typedef int nl_item;

__BEGIN_DECLS
nl_catd catopen(const char *, int);
char *catgets(nl_catd, int, int, const char *);
int catclose(nl_catd);
__END_DECLS
#endif
