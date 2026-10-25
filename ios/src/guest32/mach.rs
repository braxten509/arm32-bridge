//! Mach calls for 32-bit games: memory (vm_*), thread ports and state, the
//! port and exception-port calls crash reporters make, and the pthread
//! stack queries.
//!
//! Memory is the guest's own: vm_allocate hands out zeroed, page-aligned
//! guest blocks the way the translator's mmap does, and vm_read_overwrite
//! checks that both ranges are guest memory (KERN_INVALID_ADDRESS
//! otherwise, which is what crash reporters count on to read safely). A
//! thread port is the host thread's id, as on the 64-bit layer.

use arm32_bridge::imports::{self, threads};
use arm32_bridge::runtime::{self, mem, ArgSource, Args, GuestCpu, Handler, Ret, Uc};
use arm32_bridge::{handler, mem as layout};

use super::function;

const KERN_SUCCESS: u32 = 0;
const KERN_INVALID_ADDRESS: u32 = 1;
const KERN_NO_SPACE: u32 = 3;
const KERN_INVALID_ARGUMENT: u32 = 4;
const KERN_FAILURE: u32 = 5;
const KERN_INVALID_RIGHT: u32 = 17;
const MACH_SEND_INVALID_DEST: u32 = 0x1000_0003;

const VM_FLAGS_ANYWHERE: u32 = 1;
const MACH_PORT_RIGHT_SEND: u32 = 0;
const MACH_PORT_RIGHT_DEAD_NAME: u32 = 4;
/// ARM_THREAD_STATE: r0-r12, sp, lr, pc, cpsr.
const ARM_THREAD_STATE: u32 = 1;
const ARM_THREAD_STATE_COUNT: u32 = 17;

/// Runs one of the translator's Android handlers with these argument words
/// (the ones past r3 go on a scratch stack).
fn android(uc: &mut Uc, name: &str, words: &[u32]) -> Ret {
    let handler: Handler = imports::handler_for(name).unwrap_or_else(|| panic!("no Android handler for {name}"));
    let mut registers = [0; 4];
    let stack = super::alloc(16);
    for (i, word) in words.iter().enumerate() {
        match registers.get_mut(i) {
            Some(register) => *register = *word,
            None => mem().write_u32(stack + (i as u32 - 4) * 4, *word),
        }
    }
    let result = handler(uc, &mut Args::new(registers, stack));
    super::free(stack);
    result
}

/// Whether [start, start + length) is guest memory that is there.
fn mapped(start: u32, length: u32) -> bool {
    let Some(end) = start.checked_add(length) else { return false };
    start != 0 && layout::layout().regions.iter().any(|&(from, to, _)| from <= start && end <= to)
}

fn this_thread_port() -> u32 {
    unsafe { libc::gettid() as u32 }
}

/// pthread_get_stackaddr_np / _stacksize_np: (top, size) of `thread`'s guest stack.
fn stack(uc: &Uc, thread: u32) -> (u32, u32) {
    if thread == threads::current_handle() {
        let data = uc.get_data();
        let size = if data.id == 1 { runtime::MAIN_STACK_SIZE } else { runtime::THREAD_STACK_SIZE };
        return (data.stack_top, size);
    }
    threads::guest_stack(thread).unwrap_or((0, 0))
}

pub fn register() {
    function("mach_thread_self", handler!(|uc, a| Ret::U32(this_thread_port())).unwrap());
    // vm_allocate(task, vm_address_t *address, size, flags)
    function(
        "vm_allocate",
        handler!(|uc, a| {
            let (_task, address, size, flags) = (a.u32(), a.u32(), a.u32(), a.u32());
            let (want, map_flags) = if flags & VM_FLAGS_ANYWHERE != 0 {
                (0, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS)
            } else {
                (mem().read_u32(address), libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED)
            };
            let protection = (libc::PROT_READ | libc::PROT_WRITE) as u32;
            match android(uc, "mmap", &[want, size, protection, map_flags as u32, u32::MAX, 0]) {
                Ret::U32(block) if block != u32::MAX => {
                    mem().write_u32(address, block);
                    Ret::U32(KERN_SUCCESS)
                }
                _ => Ret::U32(KERN_NO_SPACE),
            }
        })
        .unwrap(),
    );
    function(
        "vm_deallocate",
        handler!(|uc, a| {
            let (_task, address, size) = (a.u32(), a.u32(), a.u32());
            android(uc, "munmap", &[address, size]);
            Ret::U32(KERN_SUCCESS)
        })
        .unwrap(),
    );
    // vm_protect(task, address, size, set_maximum, protection): VM_PROT_* are PROT_*.
    function(
        "vm_protect",
        handler!(|uc, a| {
            let (_task, address, size, _set_maximum, protection) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            if !mapped(address, size) {
                return Ret::U32(KERN_INVALID_ADDRESS);
            }
            android(uc, "mprotect", &[address, size, protection & 7]);
            Ret::U32(KERN_SUCCESS)
        })
        .unwrap(),
    );
    // vm_read_overwrite(task, address, size, data, vm_size_t *out_size)
    function(
        "vm_read_overwrite",
        handler!(|uc, a| {
            let (_task, address, size, data, out_size) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            if !mapped(address, size) || !mapped(data, size) {
                return Ret::U32(KERN_INVALID_ADDRESS);
            }
            let m = mem();
            unsafe { std::ptr::copy(m.host(address), m.host(data), size as usize) };
            if out_size != 0 {
                m.write_u32(out_size, size);
            }
            Ret::U32(KERN_SUCCESS)
        })
        .unwrap(),
    );
    // Ports handed out (task, host, threads) are plain send rights with
    // nothing behind them, so letting go of one always works.
    function("mach_port_deallocate", handler!(|uc, a| Ret::U32(KERN_SUCCESS)).unwrap());
    function(
        "mach_port_mod_refs",
        handler!(|uc, a| {
            let (_task, _name, right) = (a.u32(), a.u32(), a.u32());
            Ret::U32(if matches!(right, MACH_PORT_RIGHT_SEND | MACH_PORT_RIGHT_DEAD_NAME) { KERN_SUCCESS } else { KERN_INVALID_RIGHT })
        })
        .unwrap(),
    );
    // Android has no Mach ports, messages, exception ports or shared memory
    // entries: these fail as Apple's do when refused, and crash reporters give up.
    for name in [
        "mach_port_allocate",
        "mach_port_insert_right",
        "task_set_exception_ports",
        "task_swap_exception_ports",
        "thread_set_exception_ports",
        "thread_swap_exception_ports",
        "vm_map",
        "mach_make_memory_entry_64",
    ] {
        function(name, |_uc, _a| Ret::U32(KERN_FAILURE));
    }
    // No port can receive here, so every send's destination is invalid.
    function("mach_msg", handler!(|uc, a| Ret::U32(MACH_SEND_INVALID_DEST)).unwrap());
    // Guest threads run inside the translator and can't be listed or stopped
    // from outside it (their registers live in the translator's state).
    for name in ["task_threads", "thread_suspend", "thread_resume"] {
        function(name, |_uc, _a| Ret::U32(KERN_FAILURE));
    }
    // thread_get_state(thread, flavor, state, count *): the calling thread's
    // own registers at the call (pc: where it returns to); others can't be stopped.
    function(
        "thread_get_state",
        handler!(|uc, a| {
            let (thread, flavor, state, count) = (a.u32(), a.u32(), a.u32(), a.u32());
            let m = mem();
            if flavor != ARM_THREAD_STATE || state == 0 || count == 0 || m.read_u32(count) < ARM_THREAD_STATE_COUNT {
                return Ret::U32(KERN_INVALID_ARGUMENT);
            }
            if thread != this_thread_port() {
                return Ret::U32(KERN_FAILURE);
            }
            for register in 0..15u32 {
                m.write_u32(state + register * 4, uc.reg(register as usize));
            }
            let resume = uc.reg(14);
            m.write_u32(state + 15 * 4, resume & !1);
            m.write_u32(state + 16 * 4, 0x10 | (resume & 1) << 5);
            m.write_u32(count, ARM_THREAD_STATE_COUNT);
            Ret::U32(KERN_SUCCESS)
        })
        .unwrap(),
    );
    // Apple's stack address is the top of the stack (it grows down from there).
    function("pthread_get_stackaddr_np", handler!(|uc, a| Ret::U32(stack(uc, a.u32()).0)).unwrap());
    function("pthread_get_stacksize_np", handler!(|uc, a| Ret::U32(stack(uc, a.u32()).1)).unwrap());
    super::data_words("vm_page_size", vec![layout::PAGE]);
    // NDR_record: the data representation Mach messages describe (little-endian ints, IEEE floats).
    super::data_bytes("NDR_record", vec![0, 0, 0, 0, 1, 0, 0, 0], 4);
}
