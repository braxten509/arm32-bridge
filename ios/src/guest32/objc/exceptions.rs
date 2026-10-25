//! Objective-C exceptions for 32-bit games, made as Apple's runtime makes
//! them on armv7, where exceptions unwind with SjLj through the guest C++
//! runtime (libc++abi in libguest32cxx.so):
//!
//! - `objc_exception_throw(object)` allocates a C++ exception holding
//!   `{ object, type_info }` and goes on into the guest's `__cxa_throw`
//!   (as a tail call: unwinding stays in guest code).
//! - The type_info's vtable is `objc_ehtype_vtable`: libc++abi asks a
//!   catch clause through its `can_catch` slot, answered by [`do_catch`]
//!   (the thrown object's class or a superclass, or `@catch (id)`).
//! - `__objc_personality_v0` is the C++ personality and `objc_begin_catch`
//!   / `objc_end_catch` / `objc_exception_rethrow` / `objc_terminate` are
//!   libc++abi's own functions: the program's imports bind straight to
//!   them (Apple's objc4 forwards to the same functions).
//! - The runtime's terminate handler gives an Objective-C exception nobody
//!   caught to the game's NSSetUncaughtExceptionHandler function, then the
//!   program stops, as the 64-bit layer's `fw::exception::uncaught` does.

use super::functions::{class_name, describe_exception, superclass, Ctx};
use arm32_bridge::runtime::{self, mem, GuestCpu, Ret, Uc};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

/// Program imports that are the guest C++ runtime's functions: (import, export).
const ALIASES: [(&str, &str); 5] = [
    ("__objc_personality_v0", "__gxx_personality_sj0"),
    ("objc_begin_catch", "__cxa_begin_catch"),
    ("objc_end_catch", "__cxa_end_catch"),
    ("objc_exception_rethrow", "__cxa_rethrow"),
    ("objc_terminate", "_ZSt9terminatev"),
];

/// Engine-level functions wire.rs registers: throw, the vtable's two
/// answers, the terminate handler and the barrier's personality.
pub const THROW: &str = "objc_exception_throw";
pub const ANSWER_FALSE: &str = "gamebridge_objc_ehtype_false";
pub const DO_CATCH: &str = "gamebridge_objc_ehtype_do_catch";
pub const TERMINATE: &str = "gamebridge_objc_terminate";
pub const BARRIER: &str = "gamebridge_sjlj_barrier";

/// The game's uncaught exception handler (a guest function; 0: none).
static UNCAUGHT: AtomicU32 = AtomicU32::new(0);
/// The terminate handler before ours (called for C++ exceptions).
static OLD_TERMINATE: AtomicU32 = AtomicU32::new(0);

pub fn set_uncaught_handler(handler: u32) {
    UNCAUGHT.store(handler, Ordering::SeqCst);
}

pub fn uncaught_handler() -> u32 {
    UNCAUGHT.load(Ordering::SeqCst)
}

fn export(name: &str) -> Option<u32> {
    runtime::shared().guest_exports.get(name).copied()
}

fn words(values: &[u32]) -> u32 {
    let at = super::super::alloc_permanent(values.len() as u32 * 4, 4);
    for (i, value) in values.iter().enumerate() {
        mem().write_u32(at + 4 * i as u32, *value);
    }
    at
}

/// `objc_ehtype_vtable` (objc4's layout): offset and RTTI, two destructors,
/// two old type_info slots, then can_catch and three more lookups that
/// answer false. Guest code points at it plus 8.
pub fn vtable() -> u32 {
    static VTABLE: OnceLock<u32> = OnceLock::new();
    *VTABLE.get_or_init(|| {
        let no = runtime::stub_for(ANSWER_FALSE);
        // A real class type_info as its RTTI, so a C++ catch clause's
        // dynamic_cast of an Objective-C exception's type finds nothing
        // instead of reading through null.
        let rtti = export("_ZTISt9type_info").unwrap_or(0);
        words(&[0, rtti, no, no, no, no, runtime::stub_for(DO_CATCH), no, no, no])
    })
}

/// `OBJC_EHTYPE_id`, the type_info of `@catch (id e)`: catches every
/// Objective-C exception.
pub fn id_type() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| words(&[vtable() + 8, super::super::cstring_permanent("id"), 0]))
}

/// A framework class's type_info (`OBJC_EHTYPE_$_NSException`).
pub fn framework_type(name: &'static str, class: u32) -> u32 {
    words(&[vtable() + 8, super::super::cstring_permanent(name), class])
}

/// Binds the catch functions to the guest C++ runtime and installs the
/// terminate handler, as Apple's runtime does when it starts. Runs once the
/// C++ runtime is loaded, before the program is.
pub fn setup(uc: &mut Uc) -> Result<(), String> {
    {
        let mut shared = runtime::shared();
        for (import, target) in ALIASES {
            let address = *shared.guest_exports.get(target).ok_or_else(|| format!("the C++ runtime has no {target}"))?;
            shared.guest_exports.entry(import.to_string()).or_insert(address);
        }
    }
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if !INSTALLED.swap(true, Ordering::SeqCst) {
        let set_terminate = export("_ZSt13set_terminatePFvvE").ok_or("the C++ runtime has no std::set_terminate")?;
        let old = runtime::call_guest(uc, set_terminate, &[runtime::stub_for(TERMINATE)])? as u32;
        OLD_TERMINATE.store(old, Ordering::SeqCst);
    }
    Ok(())
}

/// objc_exception_throw(object): makes Apple's exception record in a C++
/// exception and goes on into `__cxa_throw(record, &record->type_info, 0)`.
pub fn throw(uc: &mut Uc, c: &Ctx, object: u32) -> Ret {
    let (Some(allocate), Some(cxa_throw)) = (export("__cxa_allocate_exception"), export("__cxa_throw")) else {
        runtime::stop_with_fault(uc, format!("the game threw an Objective-C exception ({}), with no C++ runtime", describe_exception(c, object)));
        return Ret::Void;
    };
    let record = match runtime::call_guest(uc, allocate, &[16]) {
        Ok(record) => record as u32,
        Err(error) => {
            runtime::stop_with_fault(uc, format!("objc_exception_throw: {error}"));
            return Ret::Void;
        }
    };
    let class = if object == 0 { 0 } else { c.word(object) };
    let name = c.name(&class_name(c, class));
    for (i, word) in [object, vtable() + 8, name, class].iter().enumerate() {
        mem().write_u32(record + 4 * i as u32, *word);
    }
    uc.set_reg(0, record);
    uc.set_reg(1, record + 4);
    uc.set_reg(2, 0);
    // The stand-in returns to lr: __cxa_throw never comes back (SjLj
    // unwinding finds the handler through the registered contexts).
    uc.set_reg(runtime::LR, cxa_throw);
    Ret::Jumped
}

/// The type_info vtable's can_catch: `catch_type` is a catch clause's
/// Objective-C type_info, `thrown_type` the thrown exception's,
/// `adjusted` points at the thrown object's address. As in objc4, the
/// address is replaced by the exception object itself (what the clause
/// gets from objc_begin_catch).
pub fn do_catch(c: &Ctx, catch_type: u32, thrown_type: u32, adjusted: u32) -> bool {
    if thrown_type == 0 || c.word(thrown_type) != vtable() + 8 {
        return false; // a C++ exception
    }
    let object = c.word(c.word(adjusted));
    c.set_word(adjusted, object);
    if catch_type == id_type() {
        return true;
    }
    let wanted = c.word(catch_type + 8);
    let wanted_name = if wanted == 0 { c.cstr(c.word(catch_type + 4)) } else { String::new() };
    let mut class = if object == 0 { 0 } else { c.word(object) };
    for _ in 0..64 {
        if class == 0 {
            break;
        }
        if class == wanted || (wanted == 0 && class_name(c, class) == wanted_name) {
            return true;
        }
        class = superclass(c, class);
    }
    false
}

/// A context on the guest's SjLj list for one host→guest call: unwinding
/// stops there, so a game exception never longjmps past host frames into
/// the game code that called the host (that code's call into the host
/// would never return). Its personality catches everything; the context's
/// jump buffer lands on the call's return trap, ending the call.
pub struct Barrier {
    at: u32,
    top: u32,
    prev: u32,
}

thread_local! {
    /// Barrier contexts free for reuse (guest memory, never freed).
    static FREE_BARRIERS: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// _Unwind_FunctionContext: prev, resumeLocation, resumeParameters[4],
/// personality, lsda, then the jump buffer (r7, pc, sp).
const CONTEXT_SIZE: u32 = 48;
const ESCAPED: u32 = 8;
const PERSONALITY: u32 = 24;
const JUMP_BUFFER: u32 = 32;

impl Barrier {
    /// Registers a barrier for the guest call about to be made on `uc`.
    pub fn push(uc: &Uc) -> Barrier {
        let at = FREE_BARRIERS.with(|f| f.borrow_mut().pop()).unwrap_or_else(|| super::super::alloc_permanent(CONTEXT_SIZE, 8));
        // Slot 18 of the thread block: the top of the thread's SjLj list.
        let top = uc.get_data().tls_block + 18 * 4;
        let memory = mem();
        let prev = memory.read_u32(top);
        unsafe { std::ptr::write_bytes(memory.host(at), 0, CONTEXT_SIZE as usize) };
        memory.write_u32(at, prev);
        memory.write_u32(at + PERSONALITY, runtime::stub_for(BARRIER));
        memory.write_u32(at + JUMP_BUFFER + 4, runtime::next_return_trap(uc));
        memory.write_u32(at + JUMP_BUFFER + 8, uc.get_data().stack_top - 64);
        memory.write_u32(top, at);
        Barrier { at, top, prev }
    }

    /// Unregisters it after the call: the exception that escaped the call
    /// (its _Unwind_Exception), if one did.
    pub fn pop(self) -> Option<u32> {
        let memory = mem();
        let escaped = memory.read_u32(self.at + ESCAPED);
        memory.write_u32(self.top, self.prev);
        FREE_BARRIERS.with(|f| f.borrow_mut().push(self.at));
        (escaped != 0).then_some(escaped)
    }
}

/// The barrier's personality(version, actions, class, exception, context):
/// found as the handler in the search phase; in the unwind phase it keeps
/// the exception and has the unwinder jump to the context (_URC_INSTALL_CONTEXT).
pub fn barrier_personality(actions: u32, exception: u32, context: u32) -> u32 {
    const SEARCH_PHASE: u32 = 1;
    if actions & SEARCH_PHASE != 0 {
        return 6; // _URC_HANDLER_FOUND
    }
    mem().write_u32(context + ESCAPED, exception);
    7
}

/// An exception that escaped a host→guest call (`exception`, from
/// [`Barrier::pop`]): the Objective-C object it carries, after which it is
/// finished with (caught and ended, as a catch block would). Anything else
/// (a C++ exception), or one no host code will carry on (`carried`: the
/// call isn't answering a game message), ends the program as an uncaught
/// exception does.
pub fn take_escaped(uc: &mut Uc, c: &Ctx, exception: u32, carried: bool) -> Result<u32, String> {
    let (Some(begin), Some(current_type), Some(end)) = (export("__cxa_begin_catch"), export("__cxa_current_exception_type"), export("__cxa_end_catch"))
    else {
        return Err("a game exception escaped, with no C++ runtime".into());
    };
    runtime::call_guest(uc, begin, &[exception])?;
    let thrown_type = runtime::call_guest(uc, current_type, &[])? as u32;
    // objc_exception_throw's record { object, type_info... }: the type_info is the record plus 4.
    let objc = thrown_type != 0 && c.word(thrown_type) == vtable() + 8;
    if !objc || !carried {
        terminate(uc, c);
        return Err(uc.get_data().fault.clone().unwrap_or_else(|| "an uncaught game exception".into()));
    }
    let object = c.word(thrown_type - 4);
    runtime::call_guest(uc, end, &[])?;
    Ok(object)
}

/// The terminate handler: an Objective-C exception nobody caught goes to
/// the game's uncaught exception handler, then the program stops; anything
/// else goes to the handler before ours.
pub fn terminate(uc: &mut Uc, c: &Ctx) -> Ret {
    let thrown_type = match export("__cxa_current_exception_type").map(|f| runtime::call_guest(uc, f, &[])) {
        Some(Ok(t)) => t as u32,
        _ => 0,
    };
    if thrown_type != 0 && c.word(thrown_type) == vtable() + 8 {
        let object = c.word(thrown_type - 4);
        let handler = uncaught_handler();
        if handler != 0 {
            crate::log!("uncaught Objective-C exception: calling the game's uncaught exception handler");
            if let Err(error) = runtime::call_guest(uc, handler, &[object]) {
                crate::log!("the game's uncaught exception handler failed: {error}");
            }
        }
        runtime::stop_with_fault(uc, format!("uncaught Objective-C exception: {}", describe_exception(c, object)));
        return Ret::Void;
    }
    let old = OLD_TERMINATE.load(Ordering::SeqCst);
    if old != 0 {
        let _ = runtime::call_guest(uc, old, &[]);
    }
    runtime::stop_with_fault(uc, "std::terminate: an uncaught C++ exception".into());
    Ret::Void
}
