//! Connects the Objective-C bridge to the translator and the 32-bit runner:
//! guest memory and guest calls ([`Translator`]), one handler for every
//! Objective-C runtime function, the data imports the game's classes and
//! constants bind to, and setting up the game's classes after loading.

use super::bridge::{dispatch_from_host, install, is_host_object, run_loads as run_guest_loads, Bridge, GuestSide, Special};
use super::functions::{call_function, functions, Ctx, HOST_FUNCTIONS};
use super::mirror;
use arm32_bridge::ios32::Program;
use arm32_bridge::runtime::{self, ArgSource, Args, Ret, Uc};
use guest32_image::{read_objc, ImageMemory, ObjcMetadata32, ObjcSections};
use std::cell::{Cell, RefCell};
use std::sync::{Mutex, OnceLock};

static BRIDGE: OnceLock<Mutex<Bridge>> = OnceLock::new();
static METADATA: OnceLock<ObjcMetadata32> = OnceLock::new();

thread_local! {
    /// The engine of the handler running on this thread, for nested guest calls.
    static CURRENT_UC: Cell<*mut Uc> = const { Cell::new(std::ptr::null_mut()) };
}

/// Guest memory and guest code through the translator.
pub struct Translator;

impl GuestSide for Translator {
    fn read(&self, addr: u32, len: u32) -> Vec<u8> {
        runtime::mem().bytes(addr, len as usize).to_vec()
    }
    fn write(&self, addr: u32, data: &[u8]) {
        runtime::mem().write_bytes(addr, data)
    }
    fn alloc(&self, size: u32) -> u32 {
        super::super::alloc(size.max(1))
    }
    fn free(&self, addr: u32) {
        super::super::free(addr)
    }
    fn alloc_permanent(&self, size: u32) -> u32 {
        super::super::alloc_permanent(size.max(1), 8)
    }
    fn host_address(&self, addr: u32) -> usize {
        runtime::mem().host(addr) as usize
    }
    fn guest_address(&self, host: usize) -> Option<u32> {
        let memory = runtime::mem();
        let base = memory.host(0) as usize;
        (host >= base && host - base < 1 << 32).then(|| memory.guest(host as *const u8))
    }
    fn call(&self, imp: u32, words: &[u32]) -> Result<u64, String> {
        let _gil = runtime::hold_gil();
        let current = CURRENT_UC.with(|c| c.get());
        let uc = if current.is_null() { runtime::engine() } else { unsafe { &mut *current } };
        runtime::call_guest(uc, imp, words)
    }
    fn without_guest_lock(&self, wait: &mut dyn FnMut()) {
        runtime::without_gil(wait)
    }
}

fn bridge() -> Option<&'static Mutex<Bridge>> {
    BRIDGE.get()
}

/// Runs `f` with this handler's engine remembered for nested guest calls.
fn with_engine<R>(uc: &mut Uc, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT_UC.with(|c| c.replace(uc as *mut Uc));
    let result = f();
    CURRENT_UC.with(|c| c.set(previous));
    result
}

/// The handler behind every Objective-C runtime function.
fn objc_handler(uc: &mut Uc, args: &mut Args) -> Ret {
    let name = uc.get_data().current_call;
    let Some(bridge) = bridge() else {
        runtime::stop_with_fault(uc, format!("{name} called before the Objective-C bridge was set up"));
        return Ret::Void;
    };
    let read = RefCell::new((args, Vec::<u32>::new()));
    let words = |i: u32| {
        let mut state = read.borrow_mut();
        while state.1.len() <= i as usize {
            let value = state.0.u32();
            state.1.push(value);
        }
        state.1[i as usize]
    };
    let result = with_engine(uc, || call_function(&Ctx { bridge, g: &Translator }, name, &words));
    match result {
        Ok(value) => Ret::U64(value),
        Err(error) => {
            runtime::stop_with_fault(uc, format!("{name}: {error}"));
            Ret::Void
        }
    }
}

/// UIApplicationMain(argc, argv, principalClassName, delegateClassName):
/// the host's run loop, with the class names as host strings.
fn ui_application_main(uc: &mut Uc, args: &mut Args) -> Ret {
    let (argc, argv, principal, delegate) = (args.u32(), args.u32(), args.u32(), args.u32());
    let Some(bridge) = bridge() else {
        runtime::stop_with_fault(uc, "UIApplicationMain before the Objective-C bridge was set up".into());
        return Ret::Void;
    };
    let host_strings = |guest: u32| -> Result<usize, String> {
        bridge.lock().unwrap_or_else(|e| e.into_inner()).host_object_for(&Translator, guest)
    };
    let (principal, delegate) = match (host_strings(principal), host_strings(delegate)) {
        (Ok(p), Ok(d)) => (p, d),
        (Err(e), _) | (_, Err(e)) => {
            runtime::stop_with_fault(uc, format!("UIApplicationMain: {e}"));
            return Ret::Void;
        }
    };
    let memory = runtime::mem();
    let host_argv: Vec<usize> =
        (0..argc).map(|i| memory.host(memory.read_u32(argv + 4 * i)) as usize).chain([0]).collect();
    let Some((main, _)) = crate::symbols::lookup("_UIApplicationMain") else {
        runtime::stop_with_fault(uc, "the host layer has no UIApplicationMain".into());
        return Ret::Void;
    };
    let mut call = super::convert::HostArgs::default();
    call.x[..4].copy_from_slice(&[u64::from(argc), host_argv.as_ptr() as u64, principal as u64, delegate as u64]);
    crate::log!("32-bit UIApplicationMain: handing over to the host run loop");
    let result = with_engine(uc, || mirror::call_host(main, &call));
    Ret::U32(result.x[0] as u32)
}

/// Registers the Objective-C runtime functions (before loading).
pub fn register() {
    for name in functions().iter().map(|f| f.0).chain(HOST_FUNCTIONS.iter().map(|f| f.0)) {
        super::super::function(name, objc_handler);
    }
    super::super::function("UIApplicationMain", ui_application_main);
}

/// Reads the program's Objective-C metadata and registers the data its
/// binds need (framework classes, block and constant-string classes).
/// Runs after `prepare`, before `load`.
pub fn register_program(program: &Program) -> Result<(), String> {
    if program.plan.slide != 0 {
        return Err(format!(
            "this 32-bit program loads at a slide ({:#x}); its Objective-C metadata is only read unslid so far",
            program.plan.slide
        ));
    }
    let memory = ImageMemory::new(&program.image, &program.file).map_err(|e| e.to_string())?;
    let md = read_objc(&memory, &ObjcSections::from_image(&program.image)).map_err(|e| format!("Objective-C metadata: {e}"))?;
    let bridge = Bridge::new(&md)?;
    let _ = METADATA.set(md);
    if BRIDGE.set(Mutex::new(bridge)).is_err() {
        return Err("the Objective-C bridge was already set up".into());
    }
    super::super::fw::set_object_bridge(&FwObjects);
    let mut names: Vec<&str> = program.image.binds.iter().map(|b| b.symbol.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    for symbol in names {
        let c_name = arm32_bridge::ios32::c_name(symbol).to_string();
        if let Some(class) = symbol.strip_prefix("_OBJC_CLASS_$_") {
            let class = class.to_string();
            super::super::data(&c_name, move || with_bridge(|b| b.framework_class(&Translator, &class).0));
        } else if let Some(class) = symbol.strip_prefix("_OBJC_METACLASS_$_") {
            let class = class.to_string();
            super::super::data(&c_name, move || with_bridge(|b| b.framework_class(&Translator, &class).1));
        }
    }
    // String literals' isa: the guest copy of NSString, so messages, isa
    // checks and class names work; a literal not seen before gets a host
    // string with its text.
    super::super::data("__CFConstantStringClassReference", || {
        with_bridge(|b| {
            let class = b.framework_class(&Translator, "NSString").0;
            b.set_special(class, Special::ConstantString);
            class
        })
    });
    for (symbol, kind) in [
        ("_NSConcreteStackBlock", Special::StackBlock),
        ("_NSConcreteMallocBlock", Special::MallocBlock),
        ("_NSConcreteGlobalBlock", Special::GlobalBlock),
    ] {
        super::super::data(symbol, move || {
            let isa = super::super::alloc_permanent(64, 8);
            with_bridge(|b| b.set_special(isa, kind));
            isa
        });
    }
    // Object constants (NSString *const UIApplicationDidBecomeActiveNotification,
    // NSLocalizedDescriptionKey, ...): when the host has the variable and it
    // holds a host object, the guest variable holds that object's proxy.
    let mut names: Vec<&str> = program.image.binds.iter().map(|b| b.symbol.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    for symbol in names {
        let c_name = arm32_bridge::ios32::c_name(symbol).to_string();
        if symbol.starts_with("_OBJC_") || arm32_bridge::ios32::provides_data(&c_name) {
            continue;
        }
        let Some(object) = host_object_constant(symbol) else { continue };
        super::super::data(&c_name, move || {
            let variable = super::super::alloc_permanent(4, 4);
            let guest = with_bridge(|b| b.guest_object(&Translator, object)).unwrap_or(0);
            runtime::mem().write_u32(variable, guest);
            variable
        });
    }
    // Exception type records and the empty method cache: read, never written.
    for symbol in ["_objc_empty_cache", "_objc_empty_vtable", "OBJC_EHTYPE_id", "OBJC_EHTYPE_$_NSException"] {
        super::super::data_words(symbol, vec![0; 4]);
    }
    Ok(())
}

/// The host object a host data symbol holds, if it is a variable holding an
/// object (checked as a plausible pointer before its class is read).
fn host_object_constant(symbol: &str) -> Option<usize> {
    let (address, kind) = crate::symbols::lookup(symbol)?;
    if kind != crate::symbols::Kind::Data || address == 0 {
        return None;
    }
    let value = unsafe { *(address as *const usize) };
    is_host_object(value).then_some(value)
}

/// The C framework layer's object table is the bridge's pair table, so a
/// CGImage from a C call and from -[UIImage CGImage] is one guest object.
struct FwObjects;

impl super::super::fw::ObjectBridge for FwObjects {
    fn to_host(&self, guest: u32) -> usize {
        with_bridge(|b| b.host_object_for(&Translator, guest)).unwrap_or(0)
    }
    fn to_guest(&self, host: usize) -> u32 {
        with_bridge(|b| b.guest_object(&Translator, host)).unwrap_or(0)
    }
}

fn with_bridge<R>(f: impl FnOnce(&mut Bridge) -> R) -> R {
    f(&mut bridge().expect("bridge set up in register_program").lock().unwrap_or_else(|e| e.into_inner()))
}

/// Defines the host mirrors of the game's classes and makes selector
/// references unique. Runs after `load`.
pub fn install_program() -> Result<(), String> {
    let (bridge, md) = (bridge().ok_or("no Objective-C bridge")?, METADATA.get().ok_or("no metadata")?);
    install(bridge, &Translator, md)?;
    super::bridge::set_block_owner(&Translator);
    mirror::set_dispatch(Box::new(|frame| {
        let Some(bridge) = BRIDGE.get() else { return };
        if let Err(error) = dispatch_from_host(bridge, &Translator, frame) {
            crate::log!("32-bit Objective-C: {error}");
            let _gil = runtime::hold_gil();
            let current = CURRENT_UC.with(|c| c.get());
            let uc = if current.is_null() { runtime::engine() } else { unsafe { &mut *current } };
            runtime::stop_with_fault(uc, error);
        }
    }));
    crate::log!(
        "32-bit Objective-C: {} game classes, {} categories, {} selector references",
        md.classes.len(),
        md.categories.len(),
        md.selector_refs.len()
    );
    Ok(())
}

/// Runs the game's +load methods (after binding, before its C++ initializers).
pub fn run_loads() -> Result<(), String> {
    let (bridge, md) = (bridge().ok_or("no Objective-C bridge")?, METADATA.get().ok_or("no metadata")?);
    run_guest_loads(bridge, &Translator, md)
}
