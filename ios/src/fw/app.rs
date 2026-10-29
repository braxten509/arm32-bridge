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
    crate::fw::texture_cache::start();
    // Before anything is shown: the first screen's line must find its tap.
    crate::fw::touch::schedule_test_taps();
    crate::report::stop_when_line_appears();
    let app = make_app(class_for_name(principal, "UIApplication"));
    APPLICATION.store(app, Ordering::Relaxed);
    if delegate_name != 0 {
        let delegate_class = class_for_name(delegate_name, "");
        let delegate_object = runtime::send(runtime::send(delegate_class, "alloc", &[]), "init", &[]);
        DELEGATE.store(delegate_object, Ordering::Relaxed);
    }
    // Older apps name their delegate in the main nib (owner: the app), not here.
    crate::fw::nib::load_main_nib(app);
    let delegate_object = DELEGATE.load(Ordering::Relaxed);
    if delegate_object == 0 {
        crate::log!("STOP: UIApplicationMain without a delegate (no delegate class, and no main nib gave one)");
        crate::report::finish(1);
    }
    let class_name = {
        let rt = lock();
        rt.class_name(rt.class_of(delegate_object))
    };
    crate::log!("UIApplicationMain: app {app:#x}, delegate {class_name} {delegate_object:#x}");

    // Apple's launch order: the main storyboard (window, root view
    // controller) first, then willFinish..., didFinish..., and the window
    // is shown after them unless the app showed one.
    let storyboard_window = crate::fw::storyboard::load_main_interface(delegate_object);
    finish_launching(app, delegate_object, &class_name, storyboard_window);
    let seconds: f64 = std::env::var("IOS_RUN_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    crate::report::reached_run_loop();
    crate::fw::typing::schedule_test_typing();
    if seconds > 0.0 {
        crate::log!("run loop running for {seconds} s");
        let end = {
            let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
            t.tv_sec as f64 + t.tv_nsec as f64 / 1e9 + seconds
        };
        crate::fw::runloop::run_main_loop(Some(end));
        crate::log!("run loop time is up ({} timer calls)", crate::fw::runloop::timers_fired());
        // Test switch IOS_HOME_AT_END: leave as through the Home button first
        // (games save then), so a later run can start from the saved game.
        if std::env::var_os("IOS_HOME_AT_END").is_some() {
            crate::log!("going to the background (IOS_HOME_AT_END)");
            std::thread::spawn(|| change_state(true));
            let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
            let start = t.tv_sec as f64 + t.tv_nsec as f64 / 1e9;
            // Until the game has answered (or 30 s on a slow PC run), and at
            // least 10 s: games often save on their next update, not in the call.
            for step in 1..=60 {
                crate::fw::runloop::run_main_loop(Some(start + step as f64 * 0.5));
                if step >= 20 && settled() {
                    break;
                }
            }
            crate::log!("in the background; the game {}", if settled() { "has answered" } else { "is still answering" });
        }
        crate::report::finish(0);
    }
    crate::log!("run loop running");
    crate::fw::runloop::run_main_loop(None);
    crate::report::finish(0);
}

/// The app object. As on iOS it is inactive from the start: while the main
/// nib or storyboard loads and the app finishes launching (finish_launching
/// makes it active).
fn make_app(app_class: Class) -> Id {
    STATE.store(1, Ordering::SeqCst);
    runtime::send(runtime::send(app_class, "alloc", &[]), "init", &[])
}

/// The delegate's launch messages after the main nib or storyboard:
/// willFinish..., didFinish... (or the older applicationDidFinishLaunching:),
/// the window shown, then becoming active. As on iOS the app is inactive
/// until then, and active just before -applicationDidBecomeActive:.
fn finish_launching(app: Id, delegate_object: Id, class_name: &str, storyboard_window: Id) {
    STATE.store(1, Ordering::SeqCst);
    for name in ["application:willFinishLaunchingWithOptions:", "application:didFinishLaunchingWithOptions:"] {
        if let Some(imp) = runtime::imp_for(delegate_object, runtime::sel(name)) {
            let place = crate::loader::describe_address(imp);
            // A 32-bit game's methods all start at the mirror trampoline.
            let in_game = crate::loader::try_loaded().map(|l| imp >= l.start && imp < l.end).unwrap_or(false)
                || imp == crate::guest32::objc::mirror::trampoline_address();
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
    crate::fw::storyboard::show_main_window(delegate_object, storyboard_window);
    crate::fw::foundation::post("UIApplicationDidFinishLaunchingNotification", app, 0);
    STATE.store(0, Ordering::SeqCst);
    if runtime::responds(delegate_object, "applicationDidBecomeActive:") {
        crate::log!("calling -[{class_name} applicationDidBecomeActive:]");
        runtime::send(delegate_object, "applicationDidBecomeActive:", &[app]);
    }
    crate::fw::foundation::post("UIApplicationDidBecomeActiveNotification", app, 0);
}

/// UIApplicationState: 0 active, 1 inactive, 2 background.
pub static STATE: AtomicUsize = AtomicUsize::new(0);

/// Changes of state the game hasn't finished answering (on its own thread;
/// `change_state` stops waiting after two seconds).
static UNANSWERED: AtomicUsize = AtomicUsize::new(0);

/// The game has finished answering every change of state (going to the
/// background is when games save).
pub fn settled() -> bool {
    UNANSWERED.load(Ordering::SeqCst) == 0
}

/// Tells the game what iOS tells an app on the way out (to the background)
/// or back in, on the main thread like iOS, then waits for it (up to 2 s).
pub fn change_state(to_background: bool) {
    let steps: &[(&str, &str, usize)] = if to_background {
        &[
            ("applicationWillResignActive:", "UIApplicationWillResignActiveNotification", 1),
            ("applicationDidEnterBackground:", "UIApplicationDidEnterBackgroundNotification", 2),
        ]
    } else {
        &[
            ("applicationWillEnterForeground:", "UIApplicationWillEnterForegroundNotification", 1),
            ("applicationDidBecomeActive:", "UIApplicationDidBecomeActiveNotification", 0),
        ]
    };
    let done = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let signal = done.clone();
    UNANSWERED.fetch_add(1, Ordering::SeqCst);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let app = APPLICATION.load(Ordering::Relaxed);
        let delegate_object = DELEGATE.load(Ordering::Relaxed);
        for &(method, notification, state) in steps {
            // iOS changes the state just before "did" messages and just
            // after "will" ones.
            if method.contains("Did") {
                STATE.store(state, Ordering::SeqCst);
            }
            if delegate_object != 0 && runtime::responds(delegate_object, method) {
                crate::log!("calling {method}");
                runtime::send(delegate_object, method, &[app]);
            }
            crate::fw::foundation::post(notification, app, 0);
            STATE.store(state, Ordering::SeqCst);
        }
        if !to_background {
            // Game Center asks its sign-in handler again, queued after these
            // messages and whatever work they queued.
            crate::fw::gamekit::ask_authenticate_handler_again();
        }
        UNANSWERED.fetch_sub(1, Ordering::SeqCst);
        *signal.0.lock().unwrap() = true;
        signal.1.notify_all();
    })));
    let (finished, wake) = &*done;
    let guard = finished.lock().unwrap();
    let _ = wake.wait_timeout_while(guard, std::time::Duration::from_secs(2), |finished| !*finished);
}

pub fn register(r: &mut Registry) {
    let responder = define_class("UIResponder", "NSObject", 8);
    let application = define_class("UIApplication", "UIResponder", 8);
    class_methods(application, &[("sharedApplication", shared_application as usize)]);
    methods(application, &[("delegate", delegate as usize), ("setDelegate:", set_delegate as usize)]);
    let _ = responder;
    r.function("UIApplicationMain", ui_application_main as usize);

    let pool = define_class("NSAutoreleasePool", "NSObject", 8);
    extern "C" fn pool_init(this: Id, _: Sel) -> Id {
        crate::fw::counted::pool_object_open(this);
        this
    }
    extern "C" fn pool_drain(this: Id, _: Sel) {
        crate::fw::counted::pool_object_close(this);
    }
    methods(pool, &[("init", pool_init as usize), ("drain", pool_drain as usize), ("release", pool_drain as usize)]);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The app's state each launch message saw.
    static SEEN: Mutex<Vec<(&'static str, usize)>> = Mutex::new(Vec::new());

    fn note(message: &'static str) {
        // What -[UIApplication applicationState] answers (fw/uikit.rs).
        SEEN.lock().unwrap().push((message, STATE.load(Ordering::SeqCst)));
    }

    extern "C" fn will_finish(_this: Id, _: Sel, _app: Id, _options: Id) -> bool {
        note("willFinish");
        true
    }
    extern "C" fn did_finish(_this: Id, _: Sel, _app: Id, _options: Id) -> bool {
        note("didFinish");
        true
    }
    extern "C" fn did_become_active(_this: Id, _: Sel, _app: Id) {
        note("didBecomeActive");
    }

    /// As iOS launches an app: inactive (1) from when the app object is made
    /// (the main nib or storyboard loads then), through finishing launching;
    /// active (0) from -applicationDidBecomeActive: on.
    #[test]
    fn an_app_is_inactive_until_it_becomes_active() {
        crate::run::setup();
        let delegate_class = define_class("AppTestLaunchDelegate", "NSObject", 8);
        methods(
            delegate_class,
            &[
                ("application:willFinishLaunchingWithOptions:", will_finish as usize),
                ("application:didFinishLaunchingWithOptions:", did_finish as usize),
                ("applicationDidBecomeActive:", did_become_active as usize),
            ],
        );
        let delegate_object = runtime::send(runtime::send(delegate_class, "alloc", &[]), "init", &[]);
        // The shared app object (+sharedApplication) is left alone: other
        // tests running at the same time follow the responder chain to it.
        // The state is read only by this test.
        let app = make_app(runtime::class_named("UIApplication"));
        assert_eq!(STATE.load(Ordering::SeqCst), 1, "inactive while the main nib or storyboard loads");
        finish_launching(app, delegate_object, "AppTestLaunchDelegate", 0);
        assert_eq!(*SEEN.lock().unwrap(), [("willFinish", 1), ("didFinish", 1), ("didBecomeActive", 0)]);
        assert_eq!(STATE.load(Ordering::SeqCst), 0);
    }
}
