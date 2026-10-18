//! EAGLContext: the iPhone's OpenGL ES context object. Step 2 only keeps
//! track of contexts; the real drawing surface comes in step 3, so the
//! calls that need it stop the run with a clear message.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::cell::Cell;

thread_local! {
    static CURRENT: Cell<Id> = const { Cell::new(0) };
}

extern "C" fn init_with_api(this: Id, _: Sel, api: usize) -> Id {
    if !(1..=3).contains(&api) {
        return 0;
    }
    property_set(this, "api", api as u64);
    crate::log!("OpenGL ES {api} context {this:#x} made (drawing comes in step 3)");
    this
}

extern "C" fn init_with_api_sharegroup(this: Id, s: Sel, api: usize, _sharegroup: Id) -> Id {
    init_with_api(this, s, api)
}

extern "C" fn api(this: Id, _: Sel) -> usize {
    property_get(this, "api").unwrap_or(2) as usize
}

extern "C" fn sharegroup(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn set_current(_: Class, _: Sel, context: Id) -> bool {
    CURRENT.with(|c| c.set(context));
    true
}

extern "C" fn current(_: Class, _: Sel) -> Id {
    CURRENT.with(|c| c.get())
}

extern "C" fn needs_drawing(this: Id, s: Sel) -> bool {
    let name = runtime::lock().sel_name(s);
    crate::log!("STOP: graphics: -[EAGLContext {name}] needs a real drawing surface (step 3) on {this:#x}");
    crate::report::finish(1);
}

extern "C" fn storage_from_drawable(this: Id, s: Sel, _target: usize, _drawable: Id) -> bool {
    needs_drawing(this, s)
}

extern "C" fn present(this: Id, s: Sel, _target: usize) -> bool {
    needs_drawing(this, s)
}

pub fn register(r: &mut Registry) {
    let context = define_class("EAGLContext", "NSObject", 8);
    class_methods(context, &[("setCurrentContext:", set_current as usize), ("currentContext", current as usize)]);
    methods(
        context,
        &[
            ("initWithAPI:", init_with_api as usize),
            ("initWithAPI:sharegroup:", init_with_api_sharegroup as usize),
            ("API", api as usize),
            ("sharegroup", sharegroup as usize),
            ("renderbufferStorage:fromDrawable:", storage_from_drawable as usize),
            ("presentRenderbuffer:", present as usize),
        ],
    );
    r.data("kEAGLDrawablePropertyRetainedBacking", leak(crate::fw::string::make("EAGLDrawablePropertyRetainedBacking")));
    r.data("kEAGLDrawablePropertyColorFormat", leak(crate::fw::string::make("EAGLDrawablePropertyColorFormat")));
    r.data("kEAGLColorFormatRGBA8", leak(crate::fw::string::make("EAGLColorFormatRGBA8")));
    r.data("kEAGLColorFormatRGB565", leak(crate::fw::string::make("EAGLColorFormatRGB565")));
}
