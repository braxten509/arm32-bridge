//! The app object, UIApplicationMain, autorelease pools and block classes.

use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::atomic::{AtomicUsize, Ordering};

static APPLICATION: AtomicUsize = AtomicUsize::new(0);
static DELEGATE: AtomicUsize = AtomicUsize::new(0);

extern "C" fn shared_application(_: Class, _: Sel) -> Id {
    APPLICATION.load(Ordering::Relaxed)
}

extern "C" fn delegate(_this: Id, _: Sel) -> Id {
    DELEGATE.load(Ordering::Relaxed)
}

extern "C" fn set_delegate(_this: Id, _: Sel, value: Id) {
    DELEGATE.store(value, Ordering::Relaxed);
}

extern "C" fn nothing(_this: Id, _: Sel) {}

extern "C" fn block_copy_method(this: Id, _: Sel) -> Id {
    crate::libsystem::mach::block_copy(this as *mut _) as Id
}

fn class_for_name(name: Id, fallback: &str) -> Class {
    let name = if name == 0 { fallback.to_string() } else { crate::fw::string::get(name) };
    let class = runtime::class_named(&name);
    if class == 0 {
        crate::log!("STOP: UIApplicationMain was asked for class {name}, which does not exist");
        crate::report::finish(1);
    }
    class
}

/// UIApplicationMain(argc, argv, principal class name, delegate class name).
extern "C" fn ui_application_main(_argc: i32, _argv: usize, principal: Id, delegate_name: Id) -> i32 {
    crate::fw::runloop::mark_main_thread();
    let app_class = class_for_name(principal, "UIApplication");
    let app = runtime::send(runtime::send(app_class, "alloc", &[]), "init", &[]);
    APPLICATION.store(app, Ordering::Relaxed);
    if delegate_name == 0 {
        crate::log!("STOP: UIApplicationMain without a delegate class (reading it from a storyboard is not supported)");
        crate::report::finish(1);
    }
    let delegate_class = class_for_name(delegate_name, "");
    let delegate_object = runtime::send(runtime::send(delegate_class, "alloc", &[]), "init", &[]);
    DELEGATE.store(delegate_object, Ordering::Relaxed);
    let class_name = lock().class_name(delegate_class);
    crate::log!("UIApplicationMain: app {app:#x}, delegate {class_name} {delegate_object:#x}");

    for name in ["application:willFinishLaunchingWithOptions:", "application:didFinishLaunchingWithOptions:"] {
        if let Some(imp) = runtime::imp_for(delegate_object, runtime::sel(name)) {
            let place = crate::loader::describe_address(imp);
            let in_game = crate::loader::try_loaded().map(|l| imp >= l.start && imp < l.end).unwrap_or(false);
            crate::log!("calling -[{class_name} {name}] at {place}");
            if name.starts_with("application:didFinish") {
                crate::report::reached_did_finish(in_game, &place);
            }
            let result = runtime::send(delegate_object, name, &[app, 0]);
            crate::log!("-[{class_name} {name}] returned {}", result & 0xFF);
        }
    }
    if runtime::imp_for(delegate_object, runtime::sel("application:didFinishLaunchingWithOptions:")).is_none() {
        if runtime::responds(delegate_object, "applicationDidFinishLaunching:") {
            let imp = runtime::imp_for(delegate_object, runtime::sel("applicationDidFinishLaunching:")).unwrap();
            let place = crate::loader::describe_address(imp);
            crate::report::reached_did_finish(true, &place);
            runtime::send(delegate_object, "applicationDidFinishLaunching:", &[app]);
        }
    }
    crate::fw::foundation::post("UIApplicationDidFinishLaunchingNotification", app, 0);
    if runtime::responds(delegate_object, "applicationDidBecomeActive:") {
        crate::log!("calling -[{class_name} applicationDidBecomeActive:]");
        runtime::send(delegate_object, "applicationDidBecomeActive:", &[app]);
    }
    crate::fw::foundation::post("UIApplicationDidBecomeActiveNotification", app, 0);
    let seconds: f64 = std::env::var("IOS_RUN_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    crate::report::reached_run_loop();
    if seconds > 0.0 {
        crate::log!("run loop running for {seconds} s");
        let end = {
            let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
            t.tv_sec as f64 + t.tv_nsec as f64 / 1e9 + seconds
        };
        crate::fw::runloop::run_main_loop(Some(end));
        crate::log!("run loop time is up ({} timer calls)", crate::fw::runloop::timers_fired());
        crate::report::finish(0);
    }
    crate::log!("run loop running");
    crate::fw::runloop::run_main_loop(None);
    crate::report::finish(0);
}

pub fn register(r: &mut Registry) {
    let responder = define_class("UIResponder", "NSObject", 8);
    let application = define_class("UIApplication", "UIResponder", 8);
    class_methods(application, &[("sharedApplication", shared_application as usize)]);
    methods(application, &[("delegate", delegate as usize), ("setDelegate:", set_delegate as usize)]);
    let _ = responder;
    r.function("UIApplicationMain", ui_application_main as usize);

    let pool = define_class("NSAutoreleasePool", "NSObject", 8);
    methods(pool, &[("drain", nothing as usize), ("release", nothing as usize)]);

    let block = define_class("NSBlock", "NSObject", 8);
    methods(block, &[("copy", block_copy_method as usize), ("copyWithZone:", block_copy_method as usize)]);
    let stack = define_class("__NSStackBlock__", "NSBlock", 8);
    let malloc = define_class("__NSMallocBlock__", "NSBlock", 8);
    let global = define_class("__NSGlobalBlock__", "NSBlock", 8);
    crate::libsystem::mach::MALLOC_BLOCK_CLASS.store(malloc, Ordering::Relaxed);
    r.data("_NSConcreteStackBlock", stack);
    r.data("_NSConcreteMallocBlock", malloc);
    r.data("_NSConcreteGlobalBlock", global);
}
