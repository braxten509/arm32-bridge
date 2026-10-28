//! NSException. -raise throws it through objc_exception_throw, so the game's
//! @catch blocks (and C++ catch (...)) see it as on an iPhone; one nobody
//! catches goes to `uncaught`, which runs the game's
//! NSSetUncaughtExceptionHandler function before the app ends.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, lookup, Registry};

// ----- NSException: { isa, name, reason, userInfo } -----

fn fields(object: Id) -> *mut [Id; 3] {
    (object + 8) as *mut [Id; 3]
}

fn field(object: Id, index: usize) -> Id {
    unsafe { (*fields(object))[index] }
}

fn copied(object: Id) -> Id {
    if object == 0 {
        0
    } else {
        runtime::send(object, "copy", &[])
    }
}

extern "C" fn exception_init(this: Id, _: Sel, name: Id, reason: Id, user_info: Id) -> Id {
    unsafe { *fields(this) = [copied(name), copied(reason), copied(user_info)] };
    this
}

extern "C" fn exception_with_name(class: Class, _: Sel, name: Id, reason: Id, user_info: Id) -> Id {
    let object = runtime::send(class, "alloc", &[]);
    runtime::send(object, "initWithName:reason:userInfo:", &[name, reason, user_info])
}

extern "C" fn exception_name(this: Id, _: Sel) -> Id {
    field(this, 0)
}

extern "C" fn exception_reason(this: Id, _: Sel) -> Id {
    field(this, 1)
}

extern "C" fn exception_user_info(this: Id, _: Sel) -> Id {
    field(this, 2)
}

/// As on Apple, an exception describes itself by its reason.
extern "C" fn exception_description(this: Id, _: Sel) -> Id {
    match field(this, 1) {
        0 => runtime::send(this, "name", &[]),
        reason => reason,
    }
}

/// No call stack is recorded: both lists are empty.
extern "C" fn exception_call_stack(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

extern "C" fn exception_copy(this: Id, _: Sel, _zone: usize) -> Id {
    this
}

/// Throws the exception the way the game's own `@throw` does.
extern "C" fn exception_raise(this: Id, _: Sel) {
    raise(this);
}

thread_local! {
    /// How many 32-bit game messages this thread's host code is answering.
    static GAME_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The exception host code raised for the innermost of them.
    static RAISED: std::cell::Cell<Id> = const { std::cell::Cell::new(0) };
}

/// Runs host work, handing back an exception it raised (see [`raise`])
/// instead of throwing it: the bridge throws it in the 32-bit game whose
/// message the work answered, where the game's @catch can take it.
pub fn catching<R>(work: impl FnOnce() -> R) -> (R, Option<Id>) {
    let outer = RAISED.with(|r| r.replace(0));
    GAME_CALLS.with(|c| c.set(c.get() + 1));
    let result = work();
    GAME_CALLS.with(|c| c.set(c.get() - 1));
    let raised = RAISED.with(|r| r.replace(outer));
    (result, (raised != 0).then_some(raised))
}

/// Whether this thread's host code is answering a 32-bit game's message
/// (an exception it raises then goes to the game; see [`catching`]).
pub fn answering_game() -> bool {
    GAME_CALLS.with(|c| c.get()) > 0
}

/// Whether host code answering a 32-bit game's message has raised. Rust
/// frames can't be unwound like the game's, so from then on messages host
/// code sends do nothing (and give 0) until the bridge throws the
/// exception in the game: as on Apple, the work after a raise isn't done.
pub fn pending() -> bool {
    answering_game() && RAISED.with(|r| r.get()) != 0
}

/// Raises `exception` from host code: for a 32-bit game's message it is kept
/// (the first one) and the host method returns as it can (see [`pending`]);
/// otherwise it is thrown at once.
pub fn raise(exception: Id) {
    if answering_game() {
        RAISED.with(|r| {
            if r.get() == 0 {
                r.set(exception);
            }
        });
        return;
    }
    throw(exception)
}

pub fn throw(exception: Id) -> ! {
    match lookup("objc_exception_throw") {
        Some((address, _)) => {
            let throw: extern "C" fn(Id) -> ! = unsafe { std::mem::transmute(address) };
            throw(exception)
        }
        None => uncaught(exception),
    }
}

crate::libsystem::libc::variadic!(ios_exception_raise_format, 4, ios_exception_raise_format_impl);

/// +raise:format:, with the format's arguments in 8-byte stack slots.
#[no_mangle]
extern "C" fn ios_exception_raise_format_impl(class: Class, _: Sel, name: Id, format: Id, va: *const u64) {
    let reason = if format == 0 { 0 } else { string::make(&string::format_object(format, va)) };
    raise_new(class, name, reason);
}

/// +raise:format:arguments: (an arm64 va_list points at the same slots).
extern "C" fn exception_raise_format_arguments(class: Class, _: Sel, name: Id, format: Id, va: *const u64) {
    let reason = if format == 0 { 0 } else { string::make(&string::format_object(format, va)) };
    raise_new(class, name, reason);
}

fn raise_new(class: Class, name: Id, reason: Id) {
    let exception = runtime::send(class, "exceptionWithName:reason:userInfo:", &[name, reason, 0]);
    runtime::send(exception, "raise", &[]);
    // A subclass's -raise that returns: Apple throws it anyway.
    raise(exception)
}

/// The text a STOP line gives for an exception: "Name: reason".
pub fn summary(exception: Id) -> String {
    if exception == 0 {
        return "nil".into();
    }
    // class_named takes the runtime lock itself, so it comes first.
    let exception_class = runtime::class_named("NSException");
    let is_exception = {
        let rt = runtime::lock();
        rt.is_subclass(rt.class_of(exception), exception_class)
    };
    if !is_exception {
        return format!("{} ({})", string::describe(exception), string::describe_class_of(exception));
    }
    let name = string::get(runtime::send(exception, "name", &[]));
    let reason = runtime::send(exception, "reason", &[]);
    if reason == 0 {
        name
    } else {
        format!("{name}: {}", string::get(reason))
    }
}

/// An Objective-C exception nobody caught: as on an iPhone, the game's
/// uncaught exception handler (NSSetUncaughtExceptionHandler, read back
/// through NSGetUncaughtExceptionHandler) runs first, then the app ends.
pub fn uncaught(exception: Id) -> ! {
    let handler = lookup("NSGetUncaughtExceptionHandler")
        .map(|(address, _)| {
            let get: extern "C" fn() -> usize = unsafe { std::mem::transmute(address) };
            get()
        })
        .unwrap_or(0);
    if handler != 0 {
        // Named first: the game's handler often ends the app itself.
        crate::log!("uncaught Objective-C exception: {}; calling the game's uncaught exception handler", summary(exception));
        let handler: extern "C" fn(Id) = unsafe { std::mem::transmute(handler) };
        handler(exception);
    }
    crate::log!("STOP: uncaught Objective-C exception: {}", summary(exception));
    crate::report::finish(1);
}

/// The value of Foundation's NSUndefinedKeyException (key-value coding's).
pub const UNDEFINED_KEY: &str = "NSUnknownKeyException";

/// Foundation's exception names; each is a string with its own name (but
/// NSUndefinedKeyException, which is [`UNDEFINED_KEY`]).
const NAMES: [&str; 17] = [
    "NSGenericException",
    "NSRangeException",
    "NSInvalidArgumentException",
    "NSInternalInconsistencyException",
    "NSMallocException",
    "NSObjectInaccessibleException",
    "NSObjectNotAvailableException",
    "NSDestinationInvalidException",
    "NSPortTimeoutException",
    "NSInvalidSendPortException",
    "NSInvalidReceivePortException",
    "NSPortSendException",
    "NSPortReceiveException",
    "NSCharacterConversionException",
    "NSFileHandleOperationException",
    "NSParseErrorException",
    "NSInconsistentArchiveException",
];

pub fn register(r: &mut Registry) {
    for name in NAMES {
        r.data(name, leak(string::make(name)));
    }
    r.data("NSUndefinedKeyException", leak(string::make(UNDEFINED_KEY)));
    let class = define_class("NSException", "NSObject", 32);
    class_methods(
        class,
        &[
            ("exceptionWithName:reason:userInfo:", exception_with_name as usize),
            ("raise:format:", ios_exception_raise_format as usize),
            ("raise:format:arguments:", exception_raise_format_arguments as usize),
        ],
    );
    methods(
        class,
        &[
            ("initWithName:reason:userInfo:", exception_init as usize),
            ("name", exception_name as usize),
            ("reason", exception_reason as usize),
            ("userInfo", exception_user_info as usize),
            ("description", exception_description as usize),
            ("callStackSymbols", exception_call_stack as usize),
            ("callStackReturnAddresses", exception_call_stack as usize),
            ("copyWithZone:", exception_copy as usize),
            ("raise", exception_raise as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// summary() must not hold the runtime lock while it looks up
    /// NSException (that lock is not re-entrant): an uncaught exception
    /// froze the game in summary() instead of ending it with a STOP line.
    #[test]
    fn an_exception_summary_does_not_lock_itself() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let exception = runtime::send(runtime::class_named("NSException"), "exceptionWithName:reason:userInfo:", &[s("Name"), s("why"), 0]);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(summary(exception)).unwrap());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_millis(500)).as_deref(), Ok("Name: why"));
    }
}
