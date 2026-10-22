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
    throw(this);
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

fn raise_new(class: Class, name: Id, reason: Id) -> ! {
    let exception = runtime::send(class, "exceptionWithName:reason:userInfo:", &[name, reason, 0]);
    runtime::send(exception, "raise", &[]);
    // A subclass's -raise that returns: Apple throws it anyway.
    throw(exception)
}

/// The text a STOP line gives for an exception: "Name: reason".
pub fn summary(exception: Id) -> String {
    if exception == 0 {
        return "nil".into();
    }
    let is_exception = {
        let rt = runtime::lock();
        rt.is_subclass(rt.class_of(exception), runtime::class_named("NSException"))
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
        crate::log!("uncaught Objective-C exception: calling the game's uncaught exception handler");
        let handler: extern "C" fn(Id) = unsafe { std::mem::transmute(handler) };
        handler(exception);
    }
    crate::log!("STOP: uncaught Objective-C exception: {}", summary(exception));
    crate::report::finish(1);
}

/// Foundation's exception names; each is a string with its own name.
const NAMES: [&str; 18] = [
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
    "NSUndefinedKeyException",
];

pub fn register(r: &mut Registry) {
    for name in NAMES {
        r.data(name, leak(string::make(name)));
    }
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
