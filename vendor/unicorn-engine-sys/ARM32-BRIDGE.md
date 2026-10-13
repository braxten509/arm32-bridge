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
7. `include/unicorn/arm.h` + `qemu/target/arm/unicorn_arm.c` (reg_read /
   reg_write): two new registers, UC_ARM_REG_EXCLUSIVE_ADDR and
   UC_ARM_REG_EXCLUSIVE_VAL, give access to the exclusive monitor
   (`env->exclusive_addr`, -1 = none, and `env->exclusive_val`). The
   translator keeps its own monitor and moves it in and out of Unicorn around
   fallbacks and checker runs, so an LDREX on one engine and its STREX on the
   other still pair up (with guest threads running at the same time).
8. `qemu/include/exec/exec-all.h` (curr_cflags), its callers in
   `qemu/accel/tcg/{cpu-exec,tcg-runtime,translate-all}.c`,
   `qemu/include/hw/core/cpu.h` and `qemu/target/arm/unicorn_arm.c`: a
   per-engine `parallel` flag (pseudo-register UC_ARM_REG_PARALLEL). When set,
   code is translated with CF_PARALLEL: STREX/SWP become real host atomics and
   DMB/DSB real barriers, as guest threads then run at the same time. Engines
   used one thread at a time (and the translator's checker, whose write-undo
   hook does not see atomic stores) leave it off. `arm_stop_interrupt` stops
   on EXCP_ATOMIC instead of retrying the instruction forever.
9. `qemu/configure` (64-bit atomics probe): the probe used the size-suffixed
   library names (`__atomic_load_8` ...), which newer clang rejects as
   undeclared, so CONFIG_ATOMIC64 was never set and every 64-bit atomic in
   CF_PARALLEL code (LDREXD/STREXD, the kernel's cmpxchg64 helper) left with
   EXCP_ATOMIC. It now uses the generic builtins (`__atomic_load_n` ...), as
   later QEMU does.
10. `qemu/accel/tcg/atomic_template.h`: Unicorn had replaced QEMU's atomic
    exchange with a plain read then write (`ret = *haddr; *haddr = val;`) and
    the min/max helpers' compare-and-swap with a plain compare then write, on
    every compiler, not just MSVC. With guest threads running at the same time a
    store from another thread could land in between and be lost (a SWP spinlock
    stayed locked for good). xchg now uses `atomic_xchg__nocheck` and the min/max
    loops `atomic_cmpxchg__nocheck`, as upstream QEMU; MSVC keeps the old code.
