/* Game Bridge: the parts of the C++ runtime's unwinder that are not GCC's.
 *
 *  - Entry stubs for _Unwind_RaiseException, _Unwind_Resume and
 *    _Unwind_Resume_or_Rethrow that save the caller's registers, and
 *    gb_jump, which installs a frame's registers and jumps into it.
 *  - Finding a frame's unwind information: Apple's compact unwind tables
 *    (__unwind_info) or __eh_frame FDEs for the game's Mach-O code, and the
 *    ELF .eh_frame (through .eh_frame_hdr) for this runtime's own code.
 *    Any other code (Game Bridge's Rust code, the system libraries) is not
 *    unwound: an exception reaching it with no handler on the way is
 *    uncaught, so nothing that code holds (locks, memory) is skipped.
 */

#define _GNU_SOURCE 1
#include "gamebridge_unwind.h"
#include "unwind.h"
#include "unwind-pe.h"
#include "unwind-dw2-fde.h"
#include <link.h>
#include <stdatomic.h>

/* ----- Entry stubs and the jump ----- */

#define GB_SAVE_REGISTERS                     \
  "sub sp, sp, #336\n"                        \
  "stp x0, x1, [sp, #0]\n"                    \
  "stp x2, x3, [sp, #16]\n"                   \
  "stp x4, x5, [sp, #32]\n"                   \
  "stp x6, x7, [sp, #48]\n"                   \
  "stp x8, x9, [sp, #64]\n"                   \
  "stp x10, x11, [sp, #80]\n"                 \
  "stp x12, x13, [sp, #96]\n"                 \
  "stp x14, x15, [sp, #112]\n"                \
  "stp x16, x17, [sp, #128]\n"                \
  "stp x18, x19, [sp, #144]\n"                \
  "stp x20, x21, [sp, #160]\n"                \
  "stp x22, x23, [sp, #176]\n"                \
  "stp x24, x25, [sp, #192]\n"                \
  "stp x26, x27, [sp, #208]\n"                \
  "stp x28, x29, [sp, #224]\n"                \
  "str x30, [sp, #240]\n"                     \
  "add x9, sp, #336\n"                        \
  "str x9, [sp, #248]\n"                      \
  "str x30, [sp, #256]\n"                     \
  "stp d8, d9, [sp, #264]\n"                  \
  "stp d10, d11, [sp, #280]\n"                \
  "stp d12, d13, [sp, #296]\n"                \
  "stp d14, d15, [sp, #312]\n"                \
  "mov x1, sp\n"                              \
  "stp x29, x30, [sp, #-16]!\n"               \
  "mov x29, sp\n"

#define GB_ENTRY(name, impl)                  \
  __asm__ (".text\n"                          \
	   ".p2align 2\n"                     \
	   ".globl " #name "\n"               \
	   ".hidden " #name "\n"              \
	   ".type " #name ", %function\n"     \
	   #name ":\n"                        \
	   GB_SAVE_REGISTERS                  \
	   "bl " #impl "\n"                   \
	   "ldp x29, x30, [sp], #16\n"        \
	   "add sp, sp, #336\n"               \
	   "ret\n"                            \
	   ".size " #name ", . - " #name "\n")

_Unwind_Reason_Code gb_raise_exception (struct _Unwind_Exception *, struct gb_registers *);
void gb_resume (struct _Unwind_Exception *, struct gb_registers *);
_Unwind_Reason_Code gb_resume_or_rethrow (struct _Unwind_Exception *, struct gb_registers *);

GB_ENTRY (_Unwind_RaiseException, gb_raise_exception);
GB_ENTRY (_Unwind_Resume, gb_resume);
GB_ENTRY (_Unwind_Resume_or_Rethrow, gb_resume_or_rethrow);

/* x16 holds the register block; x16, x17 (scratch between calls) and x18
   (the platform register, never the game's) are not restored.  */
__asm__ (".text\n"
	 ".p2align 2\n"
	 ".globl gb_jump\n"
	 ".hidden gb_jump\n"
	 ".type gb_jump, %function\n"
	 "gb_jump:\n"
	 "mov x16, x0\n"
	 "ldp d8, d9, [x16, #264]\n"
	 "ldp d10, d11, [x16, #280]\n"
	 "ldp d12, d13, [x16, #296]\n"
	 "ldp d14, d15, [x16, #312]\n"
	 "ldp x0, x1, [x16, #0]\n"
	 "ldp x2, x3, [x16, #16]\n"
	 "ldp x4, x5, [x16, #32]\n"
	 "ldp x6, x7, [x16, #48]\n"
	 "ldp x8, x9, [x16, #64]\n"
	 "ldp x10, x11, [x16, #80]\n"
	 "ldp x12, x13, [x16, #96]\n"
	 "ldp x14, x15, [x16, #112]\n"
	 "ldr x19, [x16, #152]\n"
	 "ldp x20, x21, [x16, #160]\n"
	 "ldp x22, x23, [x16, #176]\n"
	 "ldp x24, x25, [x16, #192]\n"
	 "ldp x26, x27, [x16, #208]\n"
	 "ldp x28, x29, [x16, #224]\n"
	 "ldr x30, [x16, #240]\n"
	 "ldr x17, [x16, #248]\n"
	 "mov sp, x17\n"
	 "ldr x17, [x16, #256]\n"
	 "br x17\n"
	 ".size gb_jump, . - gb_jump\n");

/* ----- The game's Mach-O images ----- */

struct gb_image
{
  uintptr_t header;          /* the Mach-O header: compact offsets count from here */
  uintptr_t start, end;      /* the image's memory */
  const uint8_t *unwind_info;
  size_t unwind_info_size;
  const uint8_t *eh_frame;
  size_t eh_frame_size;
};

#define GB_MAX_IMAGES 16
static struct gb_image images[GB_MAX_IMAGES];
static atomic_int image_count;

/* Called once per loaded game image, before any of its code runs.  */
__attribute__ ((visibility ("default"))) int
gamebridge_unwind_add_image (uintptr_t header, uintptr_t start, uintptr_t end,
			     const uint8_t *unwind_info, size_t unwind_info_size,
			     const uint8_t *eh_frame, size_t eh_frame_size)
{
  int n = atomic_load (&image_count);
  if (n == GB_MAX_IMAGES)
    return -1;
  images[n].header = header;
  images[n].start = start;
  images[n].end = end;
  images[n].unwind_info = unwind_info;
  images[n].unwind_info_size = unwind_info_size;
  images[n].eh_frame = eh_frame;
  images[n].eh_frame_size = eh_frame_size;
  atomic_store (&image_count, n + 1);
  return 0;
}

static uint32_t
u32_at (const uint8_t *p)
{
  uint32_t v;
  memcpy (&v, p, 4);
  return v;
}

static uint16_t
u16_at (const uint8_t *p)
{
  uint16_t v;
  memcpy (&v, p, 2);
  return v;
}

/* Apple's two-level __unwind_info lookup.  */
static int
gb_compact_lookup (const struct gb_image *image, uintptr_t pc,
		   struct gb_unwind_info *info)
{
  const uint8_t *u = image->unwind_info;
  size_t size = image->unwind_info_size;
  uint32_t target, common_offset, common_count, personality_offset,
    personality_count, index_offset, index_count;
  uint32_t lo, hi, page_offset, kind, func_offset = 0, encoding = 0;
  uint32_t lsda_start, lsda_end;
  const uint8_t *index, *page;

  if (u == NULL || size < 28 || u32_at (u) != 1)
    return GB_UNWIND_NONE;
  target = (uint32_t) (pc - image->header);
  common_offset = u32_at (u + 4);
  common_count = u32_at (u + 8);
  personality_offset = u32_at (u + 12);
  personality_count = u32_at (u + 16);
  index_offset = u32_at (u + 20);
  index_count = u32_at (u + 24);
  if (index_count < 2 || index_offset + (size_t) index_count * 12 > size)
    return GB_UNWIND_NONE;
  index = u + index_offset;

  /* The last first-level entry only marks the end.  */
  if (target < u32_at (index) || target >= u32_at (index + (index_count - 1) * 12))
    return GB_UNWIND_NONE;
  lo = 0;
  hi = index_count - 1;
  while (hi - lo > 1)
    {
      uint32_t mid = (lo + hi) / 2;
      if (u32_at (index + mid * 12) <= target)
	lo = mid;
      else
	hi = mid;
    }
  page_offset = u32_at (index + lo * 12 + 4);
  lsda_start = u32_at (index + lo * 12 + 8);
  lsda_end = u32_at (index + (lo + 1) * 12 + 8);
  if (page_offset == 0 || page_offset + 8 > size)
    return GB_UNWIND_NONE;
  page = u + page_offset;
  kind = u32_at (page);

  if (kind == 2)
    {
      /* Regular page: (function offset, encoding) pairs.  */
      uint32_t entries = u16_at (page + 4), count = u16_at (page + 6);
      const uint8_t *e = page + entries;
      uint32_t a = 0, b = count;
      if (count == 0 || page_offset + entries + (size_t) count * 8 > size
	  || u32_at (e) > target)
	return GB_UNWIND_NONE;
      while (b - a > 1)
	{
	  uint32_t mid = (a + b) / 2;
	  if (u32_at (e + mid * 8) <= target)
	    a = mid;
	  else
	    b = mid;
	}
      func_offset = u32_at (e + a * 8);
      encoding = u32_at (e + a * 8 + 4);
    }
  else if (kind == 3)
    {
      /* Compressed page: 24-bit offset from the first-level entry's
	 function, 8-bit encoding index (common list, then this page's).  */
      uint32_t entries = u16_at (page + 4), count = u16_at (page + 6);
      uint32_t encodings = u16_at (page + 8), encodings_count = u16_at (page + 10);
      uint32_t base = u32_at (index + lo * 12);
      const uint8_t *e = page + entries;
      uint32_t a = 0, b = count, entry, which;
      if (count == 0 || page_offset + entries + (size_t) count * 4 > size
	  || base + (u32_at (e) & 0xFFFFFF) > target)
	return GB_UNWIND_NONE;
      while (b - a > 1)
	{
	  uint32_t mid = (a + b) / 2;
	  if (base + (u32_at (e + mid * 4) & 0xFFFFFF) <= target)
	    a = mid;
	  else
	    b = mid;
	}
      entry = u32_at (e + a * 4);
      func_offset = base + (entry & 0xFFFFFF);
      which = entry >> 24;
      if (which < common_count)
	encoding = u32_at (u + common_offset + which * 4);
      else if (which - common_count < encodings_count)
	encoding = u32_at (page + encodings + (which - common_count) * 4);
      else
	return GB_UNWIND_NONE;
    }
  else
    return GB_UNWIND_NONE;

  if (encoding == 0)
    return GB_UNWIND_NONE;   /* the linker had no unwind info for it */

  memset (info, 0, sizeof *info);
  info->encoding = encoding;
  info->func = image->header + func_offset;

  if ((encoding & GB_ARM64_MODE_MASK) == GB_ARM64_MODE_DWARF)
    {
      uint32_t at = encoding & GB_ARM64_DWARF_SECTION_OFFSET;
      if (image->eh_frame == NULL || at >= image->eh_frame_size)
	return GB_UNWIND_NONE;
      info->fde = (const struct dwarf_fde *) (image->eh_frame + at);
      return GB_UNWIND_FDE;
    }

  if (encoding & GB_UNWIND_HAS_LSDA)
    {
      /* (function offset, LSDA offset) pairs, sorted.  */
      uint32_t a = 0, b;
      if (lsda_end < lsda_start || lsda_end > size)
	return GB_UNWIND_NONE;
      b = (lsda_end - lsda_start) / 8;
      while (a < b)
	{
	  uint32_t mid = (a + b) / 2, f = u32_at (u + lsda_start + mid * 8);
	  if (f == func_offset)
	    {
	      info->lsda = image->header + u32_at (u + lsda_start + mid * 8 + 4);
	      break;
	    }
	  if (f < func_offset)
	    a = mid + 1;
	  else
	    b = mid;
	}
      if (info->lsda == 0)
	return GB_UNWIND_NONE;
    }

  {
    uint32_t p = (encoding & GB_UNWIND_PERSONALITY_MASK) >> GB_UNWIND_PERSONALITY_SHIFT;
    if (p != 0)
      {
	uintptr_t slot;
	if (p > personality_count)
	  return GB_UNWIND_NONE;
	/* An offset to the pointer (in the GOT) the loader bound.  */
	slot = image->header + u32_at (u + personality_offset + (p - 1) * 4);
	memcpy (&info->personality, (const void *) slot, sizeof info->personality);
      }
  }
  return GB_UNWIND_COMPACT;
}

/* ----- This runtime's own code (ELF) ----- */

/* Set around all of the runtime's code by the linker script in ios/build.rs.  */
extern const char gamebridge_cxxrt_text_start[] __attribute__ ((visibility ("hidden")));
extern const char gamebridge_cxxrt_text_end[] __attribute__ ((visibility ("hidden")));

/* The encoding of an FDE's addresses, from its CIE's augmentation.  */
static unsigned char
gb_fde_encoding (const struct dwarf_fde *f)
{
  const struct dwarf_cie *cie = get_cie (f);
  const unsigned char *aug = cie->augmentation;
  const unsigned char *p = aug + strlen ((const char *) aug) + 1;
  _Unwind_Word utmp;
  _Unwind_Sword stmp;

  if (aug[0] != 'z')
    return DW_EH_PE_absptr;
  p = read_uleb128 (p, &utmp);          /* code alignment */
  p = read_sleb128 (p, &stmp);          /* data alignment */
  if (cie->version == 1)
    p++;                                /* return address column */
  else
    p = read_uleb128 (p, &utmp);
  p = read_uleb128 (p, &utmp);          /* augmentation length */
  for (aug++; *aug; aug++)
    {
      if (*aug == 'R')
	return *p;
      else if (*aug == 'L')
	p++;
      else if (*aug == 'P')
	{
	  _Unwind_Ptr dummy;
	  p = read_encoded_value_with_base (*p & 0x7f, 0, p + 1, &dummy);
	}
      else if (*aug != 'S')
	break;
    }
  return DW_EH_PE_absptr;
}

struct gb_elf_search
{
  uintptr_t pc;
  const struct dwarf_fde *fde;
  uintptr_t func;
};

static int
gb_elf_callback (struct dl_phdr_info *info, size_t size, void *data)
{
  struct gb_elf_search *search = data;
  const ElfW(Phdr) *hdr_segment = NULL;
  const unsigned char *hdr, *p;
  _Unwind_Ptr eh_frame, count;
  int i, inside = 0;
  (void) size;

  for (i = 0; i < info->dlpi_phnum; i++)
    {
      const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
      uintptr_t at = info->dlpi_addr + ph->p_vaddr;
      if (ph->p_type == PT_LOAD && search->pc >= at && search->pc < at + ph->p_memsz)
	inside = 1;
      else if (ph->p_type == PT_GNU_EH_FRAME)
	hdr_segment = ph;
    }
  if (!inside)
    return 0;
  if (hdr_segment == NULL)
    return 1;
  hdr = (const unsigned char *) (info->dlpi_addr + hdr_segment->p_vaddr);
  /* version 1; the table must be the usual sorted datarel sdata4 one.  */
  if (hdr[0] != 1 || hdr[3] != (DW_EH_PE_datarel | DW_EH_PE_sdata4))
    return 1;
  /* GCC's reader adds the base to every non-pcrel value, so it is only
     given for datarel ones (relative to .eh_frame_hdr here).  */
#define GB_HDR_BASE(enc) (((enc) & 0x70) == DW_EH_PE_datarel ? (_Unwind_Ptr) hdr : 0)
  p = read_encoded_value_with_base (hdr[1], GB_HDR_BASE (hdr[1]), hdr + 4, &eh_frame);
  p = read_encoded_value_with_base (hdr[2], GB_HDR_BASE (hdr[2]), p, &count);
  if (count == 0)
    return 1;
  {
    const int32_t *table = (const int32_t *) p;
    size_t lo = 0, hi = count;
    const struct dwarf_fde *f;
    unsigned char encoding;
    _Unwind_Ptr range;
    uintptr_t start;

    if (search->pc < (uintptr_t) hdr + table[0])
      return 1;
    while (hi - lo > 1)
      {
	size_t mid = (lo + hi) / 2;
	if ((uintptr_t) hdr + table[mid * 2] <= search->pc)
	  lo = mid;
	else
	  hi = mid;
      }
    start = (uintptr_t) hdr + table[lo * 2];
    f = (const struct dwarf_fde *) ((uintptr_t) hdr + table[lo * 2 + 1]);
    encoding = gb_fde_encoding (f);
    read_encoded_value_with_base (encoding & 0x0f, 0,
				  &f->pc_begin[size_of_encoded_value (encoding)], &range);
    if (search->pc < start + range)
      {
	search->fde = f;
	search->func = start;
      }
  }
  return 1;
}

/* A game function the compact table says nothing about (no __unwind_info,
   as with -no_compact_unwind, or an entry of 0): its FDE, searched for in
   the image's __eh_frame, as Apple's unwinder does.  */
static int
gb_eh_frame_lookup (const struct gb_image *image, uintptr_t pc,
		    struct gb_unwind_info *info)
{
  const uint8_t *p = image->eh_frame, *end = p + image->eh_frame_size;

  while (p != NULL && p + 8 <= end)
    {
      uint32_t length = u32_at (p);
      const struct dwarf_fde *f = (const struct dwarf_fde *) p;
      if (length == 0 || length == 0xffffffff || p + 4 + length > end)
	break;                  /* the end, or a 64-bit record (never in Mach-O) */
      if (u32_at (p + 4) != 0)
	{
	  unsigned char encoding = gb_fde_encoding (f);
	  _Unwind_Ptr begin, range;
	  const unsigned char *q
	    = read_encoded_value_with_base (encoding, 0, f->pc_begin, &begin);
	  read_encoded_value_with_base (encoding & 0x0f, 0, q, &range);
	  if (pc >= begin && pc - begin < range)
	    {
	      memset (info, 0, sizeof *info);
	      info->fde = f;
	      info->func = begin;
	      return GB_UNWIND_FDE;
	    }
	}
      p += 4 + length;
    }
  return GB_UNWIND_NONE;
}

int
gb_find_unwind_info (void *pc_pointer, struct gb_unwind_info *info)
{
  uintptr_t pc = (uintptr_t) pc_pointer;
  int i, n = atomic_load (&image_count);

  for (i = 0; i < n; i++)
    if (pc >= images[i].start && pc < images[i].end)
      {
	int found = gb_compact_lookup (&images[i], pc, info);
	if (found == GB_UNWIND_NONE)
	  found = gb_eh_frame_lookup (&images[i], pc, info);
	return found;
      }

  if (pc >= (uintptr_t) gamebridge_cxxrt_text_start
      && pc < (uintptr_t) gamebridge_cxxrt_text_end)
    {
      struct gb_elf_search search = { pc, NULL, 0 };
      dl_iterate_phdr (gb_elf_callback, &search);
      if (search.fde == NULL)
	return GB_UNWIND_NONE;
      memset (info, 0, sizeof *info);
      info->fde = search.fde;
      info->func = search.func;
      return GB_UNWIND_FDE;
    }
  return GB_UNWIND_NONE;
}
