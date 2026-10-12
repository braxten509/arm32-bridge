# Local changes to unicorn-engine-sys 2.1.5

Copied from crates.io; two speed patches for arm32-bridge:

1. `qemu/accel/tcg/cputlb.c` (tlb_set_page_with_attrs): only regions mapped
   with UC_PROT_EXEC get TLB_NOTDIRTY. Unicorn has no dirty tracking, so every
   TLB refill made the next write run notdirty_write (a region lookup plus a
   translated-code invalidation). Data regions now take the fast write path.
2. `qemu/include/exec/cpu-defs.h`: CPU_TLB_DYN_DEFAULT_BITS 8 -> 12, so the
   software TLB covers 16 MiB instead of 1 MiB.
3. `qemu/tcg/tcg-op.c`: the exit-request helper call emitted after every
   guest load/store is skipped unless a UC_HOOK_MEM_READ/WRITE/READ_AFTER hook
   exists (arm32-bridge has none). It was 17% of the game's frame time.
