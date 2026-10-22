/* Game Bridge: settings and glue for GCC's DWARF unwinder (unwind-dw2.c),
   built as the private unwinder of the C++ runtime iPhone games use.  */

#ifndef GAMEBRIDGE_UNWIND_H
#define GAMEBRIDGE_UNWIND_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <alloca.h>

/* What GCC's target headers would say for arm64.  */
#define HIDE_EXPORTS 1                /* names stay hidden (then local) */
#define DWARF_FRAME_REGISTERS 96      /* x0-x30, sp, v0-v31 at 64-95 */
#define STACK_GROWS_DOWNWARD 1
#define EH_RETURN_STACKADJ_RTX 1      /* sp is not saved: a frame's sp is its callee's CFA */

void gamebridge_cxx_unwind_fault (int line) __attribute__ ((noreturn));
void gamebridge_cxx_unwind_stopped (void *pc);
#define gb_unwind_fault gamebridge_cxx_unwind_fault
#define gcc_assert(e) ((e) ? (void) 0 : gb_unwind_fault (__LINE__))
#define gcc_unreachable() gb_unwind_fault (__LINE__)

/* The registers an entry stub saves (layout shared with its assembly).  */
struct gb_registers
{
  uint64_t x[31];   /* x0-x30, offset 0 */
  uint64_t sp;      /* offset 248 */
  uint64_t pc;      /* offset 256: where the unwinder was called from */
  uint64_t d[8];    /* d8-d15, offset 264 */
};

/* Apple's arm64 compact unwind encoding (<mach-o/compact_unwind_encoding.h>).  */
#define GB_UNWIND_HAS_LSDA                0x40000000u
#define GB_UNWIND_PERSONALITY_MASK        0x30000000u
#define GB_UNWIND_PERSONALITY_SHIFT       28
#define GB_ARM64_MODE_MASK                0x0F000000u
#define GB_ARM64_MODE_FRAMELESS           0x02000000u
#define GB_ARM64_MODE_DWARF               0x03000000u
#define GB_ARM64_MODE_FRAME               0x04000000u
#define GB_ARM64_X19_X20                  0x00000001u
#define GB_ARM64_X21_X22                  0x00000002u
#define GB_ARM64_X23_X24                  0x00000004u
#define GB_ARM64_X25_X26                  0x00000008u
#define GB_ARM64_X27_X28                  0x00000010u
#define GB_ARM64_D8_D9                    0x00000100u
#define GB_ARM64_D10_D11                  0x00000200u
#define GB_ARM64_D12_D13                  0x00000400u
#define GB_ARM64_D14_D15                  0x00000800u
#define GB_ARM64_FRAMELESS_STACK_MASK     0x00FFF000u
#define GB_ARM64_FRAMELESS_STACK_SHIFT    12
#define GB_ARM64_DWARF_SECTION_OFFSET     0x00FFFFFFu

enum { GB_UNWIND_NONE, GB_UNWIND_COMPACT, GB_UNWIND_FDE };

struct dwarf_fde;
struct gb_unwind_info
{
  uint32_t encoding;          /* compact encoding (GB_UNWIND_COMPACT) */
  uintptr_t func;             /* start of the function */
  uintptr_t lsda;             /* compact: the function's LSDA, or 0 */
  uintptr_t personality;      /* compact: personality routine, or 0 */
  const struct dwarf_fde *fde;   /* GB_UNWIND_FDE */
};

/* How to unwind the frame containing PC: GB_UNWIND_COMPACT or
   GB_UNWIND_FDE (with INFO filled in), or GB_UNWIND_NONE for code that
   is neither the game's nor this runtime's.  */
int gb_find_unwind_info (void *pc, struct gb_unwind_info *info);

/* Loads every register in REGS, sets sp, jumps to pc.  */
void gb_jump (const struct gb_registers *regs) __attribute__ ((noreturn));

/* Phase 1 ended without a handler at the frame returning to PC.  */
#define gb_search_ended gamebridge_cxx_unwind_stopped

#endif
