# Local changes to unicorn-engine-sys 2.1.5

Copied from crates.io; speed patches and one fix for arm32-bridge:

1. `qemu/accel/tcg/cputlb.c` (tlb_set_page_with_attrs): only regions mapped
   with UC_PROT_EXEC get TLB_NOTDIRTY. Unicorn has no dirty tracking, so every
   TLB refill made the next write run notdirty_write (a region lookup plus a
   translated-code invalidation). Data regions now take the fast write path.
2. `qemu/include/exec/cpu-defs.h`: CPU_TLB_DYN_DEFAULT_BITS 8 -> 12, so the
   software TLB covers 16 MiB instead of 1 MiB.
3. `qemu/tcg/tcg-op.c`: the exit-request helper call emitted after every
   guest load/store is skipped unless a UC_HOOK_MEM_READ/WRITE/READ_AFTER hook
   exists (arm32-bridge has none). It was 17% of the game's frame time.
4. `uc.c` (uc_emu_start): when the instruction-count hook is added back after
   a run without a count, translated blocks are flushed. Blocks translated while
   counting was off have no counter call, so `count` ran past its limit
   (found by the translator's checker: a 1-instruction step ran into a callee).
5. `qemu/target/arm/translate.c` (disas_arm_insn): clears `uc->no_exit_request`
   for every ARM instruction. Only the Thumb translator sets it (inside IT
   blocks), so after translating an IT block, later ARM code was translated
   without exit checks and `count`/`uc_emu_stop` overran to the end of the block.
6. `qemu/target/arm/translate.c` (trans_YIELD, trans_WFE) and
   `qemu/target/arm/op_helper.c` (helper_yield): YIELD and WFE call the
   UC_HOOK_INTR hooks with EXCP_YIELD and carry on, instead of ending the run
   (which Unicorn reported as INSN_INVALID, so Unity's job worker threads
   stopped in their spin-wait loop). arm32-bridge hands the guest lock to
   another thread there, so a spinning thread can't starve the one it waits on.
