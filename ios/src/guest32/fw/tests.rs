//! Behavior that could silently break at the 32-bit framework boundary:
//! structs passed by value across r0-r3 and the stack, struct results
//! through the hidden pointer, host objects and handles seen as guest
//! words, and 32-bit struct layouts that hold pointers.

use arm32_bridge::imports::gles;
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, Args, GuestCpu};

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::run::setup();
        let _ = runtime::set_platform(&ios32::APPLE32);
        crate::guest32::register_all();
        runtime::init(false);
    });
}

fn alloc(size: u32) -> u32 {
    setup();
    crate::guest32::alloc(size)
}

fn cstring(text: &str) -> u32 {
    setup();
    crate::guest32::cstring(text)
}

fn f(value: f32) -> u32 {
    value.to_bits()
}

/// Calls registered function `name` with r0-r3 and stack words; returns (r0, r1).
fn call(name: &str, registers: [u32; 4], stack: &[u32]) -> (u32, u32) {
    setup();
    let _gil = runtime::hold_gil();
    let uc = runtime::engine();
    let sp = alloc(64 + stack.len() as u32 * 4);
    for (i, word) in stack.iter().enumerate() {
        mem().write_u32(sp + i as u32 * 4, *word);
    }
    let handler = ios32::function(name).unwrap_or_else(|| panic!("{name} is not registered"));
    uc.get_data_mut().current_call = Box::leak(name.to_string().into_boxed_str());
    let result = handler(uc, &mut Args::new(registers, sp));
    runtime::write_result(uc, result);
    (uc.reg(0), uc.reg(1))
}

fn floats(address: u32, count: u32) -> Vec<f32> {
    (0..count).map(|i| f32::from_bits(mem().read_u32(address + i * 4))).collect()
}

#[test]
fn structs_by_value_straddle_registers_and_stack() {
    // CGRectGetMaxY(CGRect): four floats in r0-r3, a float back in r0.
    let (max_y, _) = call("CGRectGetMaxY", [f(1.0), f(2.0), f(10.0), f(20.0)], &[]);
    assert_eq!(f32::from_bits(max_y), 22.0);
    // CGRectInset(&result, CGRect, dx, dy): the hidden result pointer takes r0,
    // so the rect is r1-r3 plus the first stack word.
    let out = alloc(16);
    call("CGRectInset", [out, f(1.0), f(2.0), f(10.0)], &[f(20.0), f(1.0), f(2.0)]);
    assert_eq!(floats(out, 4), [2.0, 4.0, 8.0, 16.0]);
    // CGAffineTransformScale(&result, CGAffineTransform (6 floats), sx, sy).
    let out = alloc(24);
    call("CGAffineTransformScale", [out, f(1.0), f(0.0), f(0.0)], &[f(1.0), f(5.0), f(6.0), f(2.0), f(3.0)]);
    assert_eq!(floats(out, 6), [2.0, 0.0, 0.0, 3.0, 5.0, 6.0]);
    // Constants are guest floats, not the host's doubles.
    setup();
    let identity = crate::guest32::data_address("CGAffineTransformIdentity").expect("CGAffineTransformIdentity");
    assert_eq!(floats(identity, 6), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
}

#[test]
fn graphics_objects_cross_as_guest_words() {
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    assert_ne!(space, 0);
    assert_eq!(call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]).0, space, "one guest word per host object");
    assert_eq!(call("CGColorSpaceGetNumberOfComponents", [space, 0, 0, 0], &[]).0, 3);
    // A bitmap context over guest memory: drawing lands in the game's pixels.
    let pixels = alloc(4 * 4 * 4);
    unsafe { std::ptr::write_bytes(mem().host(pixels), 0, 64) };
    let (context, _) = call("CGBitmapContextCreate", [pixels, 4, 4, 8], &[16, space, 1]);
    assert_ne!(context, 0);
    call("CGContextSetRGBFillColor", [context, f(1.0), f(0.0), f(0.0)], &[f(1.0)]);
    call("CGContextFillRect", [context, f(0.0), f(0.0), f(4.0)], &[f(4.0)]);
    assert_eq!(mem().bytes(pixels, 4), [255, 0, 0, 255]);
    call("CGContextRelease", [context, 0, 0, 0], &[]);
}

#[test]
fn every_advertised_gl_extension_function_is_provided() {
    setup();
    let missing: Vec<&str> = crate::fw::gl::extension_names().into_iter().filter(|name| ios32::function(name).is_none()).collect();
    assert!(missing.is_empty(), "no guest handler for {missing:?}");
}

#[test]
fn a_data_provider_runs_the_games_release_on_the_last_release_only() {
    // Guest code for the release callback: *(int*)info = 1; return.
    let code = alloc(12);
    for (i, word) in [0xE3A0_1001u32, 0xE580_1000, 0xE12F_FF1E].iter().enumerate() {
        mem().write_u32(code + i as u32 * 4, *word);
    }
    let flag = alloc(4);
    mem().write_u32(flag, 0);
    let pixels = alloc(16);
    let (provider, _) = call("CGDataProviderCreateWithData", [flag, pixels, 16, code], &[]);
    call("CGDataProviderRetain", [provider, 0, 0, 0], &[]);
    call("CGDataProviderRelease", [provider, 0, 0, 0], &[]);
    assert_eq!(mem().read_u32(flag), 0, "still held once");
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let (picture, _) = call("CGImageCreate", [2, 2, 8, 32], &[8, space, 1, provider, 0, 0, 0]);
    assert_ne!(picture, 0);
    call("CGDataProviderRelease", [provider, 0, 0, 0], &[]);
    assert_eq!(mem().read_u32(flag), 1);
    // CFRelease is a last release too.
    mem().write_u32(flag, 0);
    let (provider, _) = call("CGDataProviderCreateWithData", [flag, pixels, 16, code], &[]);
    call("CFRelease", [provider, 0, 0, 0], &[]);
    assert_eq!(mem().read_u32(flag), 1, "CFRelease ran the callback");
    // A release message runs it at once too, with no framework call after it.
    mem().write_u32(flag, 0);
    let (provider, _) = call("CGDataProviderCreateWithData", [flag, pixels, 16, code], &[]);
    crate::objc::runtime::send(super::object_to_host(provider), "release", &[]);
    assert_eq!(mem().read_u32(flag), 1, "release message ran the callback");
}

#[test]
fn openal_device_and_context_are_guest_handles() {
    let (device, _) = call("alcOpenDevice", [0; 4], &[]);
    assert!(device != 0);
    let (context, _) = call("alcCreateContext", [device, 0, 0, 0], &[]);
    assert!(context != 0, "the host knew the device from its guest handle");
    assert_eq!(call("alcGetContextsDevice", [context, 0, 0, 0], &[]).0, device);
    let (version, _) = call("alGetString", [0xB002, 0, 0, 0], &[]);
    assert_eq!(mem().string(version), "1.1");
}

#[test]
fn audio_buffer_lists_use_the_32_bit_layout() {
    // { mNumberBuffers; { mNumberChannels, mDataByteSize, mData } ... } with 4-byte pointers.
    setup();
    let data = alloc(64);
    let list = alloc(4 + 12);
    mem().write_u32(list, 1);
    mem().write_u32(list + 4, 2);
    mem().write_u32(list + 8, 64);
    mem().write_u32(list + 12, data);
    super::audio::with_host_buffer_list(list, |host| unsafe {
        let host = host as *mut u8;
        assert_eq!(*(host as *const u32), 1);
        let buffer = host.add(8);
        assert_eq!(*(buffer as *const u32), 2);
        assert_eq!(*(buffer.add(4) as *const u32), 64);
        let bytes = *(buffer.add(8) as *const *mut u8);
        *bytes = 0x5A;
        *(buffer.add(4) as *mut u32) = 32;
    });
    assert_eq!(mem().read_u32(list + 8), 32, "the size the host wrote comes back");
    assert_eq!(mem().read_u8(data), 0x5A);
}

#[test]
fn xml_text_writer_writes_the_file() {
    let path = std::env::temp_dir().join(format!("guest32-xml-{}.xml", std::process::id()));
    let (writer, _) = call("xmlNewTextWriterFilename", [cstring(path.to_str().unwrap()), 0, 0, 0], &[]);
    assert_ne!(writer, 0);
    call("xmlTextWriterStartDocument", [writer, 0, cstring("UTF-8"), 0], &[]);
    call("xmlTextWriterStartElement", [writer, cstring("save"), 0, 0], &[]);
    call("xmlTextWriterWriteAttribute", [writer, cstring("a"), cstring("1 < 2 & \"x\""), 0], &[]);
    call("xmlTextWriterWriteFormatAttribute", [writer, cstring("b"), cstring("%d-%s"), 7], &[cstring("x")]);
    call("xmlTextWriterStartElement", [writer, cstring("item"), 0, 0], &[]);
    call("xmlTextWriterEndElement", [writer, 0, 0, 0], &[]);
    call("xmlTextWriterEndElement", [writer, 0, 0, 0], &[]);
    call("xmlFreeTextWriter", [writer, 0, 0, 0], &[]);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(text, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<save a=\"1 &lt; 2 &amp; &quot;x&quot;\" b=\"7-x\"><item/></save>");
}

// ----- GL contexts -----

const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_MAP_READ_BIT: u32 = 0x0001;
const GL_MAP_WRITE_BIT: u32 = 0x0002;

/// A host EAGLContext, made the way the game's messages make one.
fn eagl_context(sharegroup: usize) -> usize {
    let class = crate::objc::runtime::class_named("EAGLContext");
    assert_ne!(class, 0, "the iPhone layer defines EAGLContext");
    let context = crate::objc::runtime::send(class, "alloc", &[]);
    let context = crate::objc::runtime::send(context, "initWithAPI:sharegroup:", &[2, sharegroup]);
    assert_ne!(context, 0, "an OpenGL ES 2 context was made");
    context
}

fn make_current(context: usize) {
    let class = crate::objc::runtime::class_named("EAGLContext");
    assert_ne!(crate::objc::runtime::send(class, "setCurrentContext:", &[context]), 0, "the context becomes current");
}

/// A filled 8-byte array buffer; returns (name, contents).
fn filled_buffer() -> (u32, [u8; 8]) {
    let names = alloc(4);
    call("glGenBuffers", [1, names, 0, 0], &[]);
    let name = mem().read_u32(names);
    assert_ne!(name, 0, "a buffer was made");
    call("glBindBuffer", [GL_ARRAY_BUFFER, name, 0, 0], &[]);
    let contents = [1, 2, 3, 4, 5, 6, 7, 8];
    let data = alloc(8);
    for (i, byte) in contents.iter().enumerate() {
        mem().write_u8(data + i as u32, *byte);
    }
    call("glBufferData", [GL_ARRAY_BUFFER, 8, data, GL_STATIC_DRAW], &[]);
    (name, contents)
}

#[test]
fn shared_eagl_contexts_share_mappings_but_not_bindings() {
    setup();
    let a = eagl_context(0);
    let b = eagl_context(a);
    make_current(a);
    let (buffer, _) = filled_buffer();
    let copy = call("glMapBufferRange", [GL_ARRAY_BUFFER, 0, 8, GL_MAP_WRITE_BIT], &[]).0;
    assert_ne!(copy, 0, "the buffer mapped");
    let written = [11, 12, 13, 14, 15, 16, 17, 18];
    for (i, byte) in written.iter().enumerate() {
        mem().write_u8(copy + i as u32, *byte);
    }

    make_current(b);
    assert_eq!(gles::bound_buffer(GL_ARRAY_BUFFER), 0, "a shared context keeps its own bindings");
    call("glBindBuffer", [GL_ARRAY_BUFFER, buffer, 0, 0], &[]);
    assert_eq!(call("glUnmapBuffer", [GL_ARRAY_BUFFER, 0, 0, 0], &[]).0, 1, "the sharegroup finds the mapping");
    let read_back = call("glMapBufferRange", [GL_ARRAY_BUFFER, 0, 8, GL_MAP_READ_BIT], &[]).0;
    assert_ne!(read_back, 0);
    assert_eq!(mem().bytes(read_back, 8), written, "the write made in the other context reached GL");
    call("glUnmapBuffer", [GL_ARRAY_BUFFER, 0, 0, 0], &[]);
    make_current(a);
}

#[test]
fn unshared_eagl_contexts_keep_their_bindings_and_mappings() {
    setup();
    let a = eagl_context(0);
    let b = eagl_context(0);
    make_current(a);
    let (buffer, contents) = filled_buffer();
    let copy = call("glMapBufferRange", [GL_ARRAY_BUFFER, 0, 8, GL_MAP_WRITE_BIT], &[]).0;
    assert_ne!(copy, 0, "the buffer mapped");
    for (i, byte) in contents.iter().enumerate() {
        mem().write_u8(copy + i as u32, *byte);
    }

    make_current(b);
    // A name the other context made is not bound here unless the game binds it.
    call("glBindBuffer", [GL_ARRAY_BUFFER, buffer, 0, 0], &[]);
    assert_eq!(gles::bound_buffer(GL_ARRAY_BUFFER), buffer);
    assert_eq!(call("glUnmapBuffer", [GL_ARRAY_BUFFER, 0, 0, 0], &[]).0, 0, "a mapping from another context is not found");
    make_current(a);
    assert_eq!(gles::bound_buffer(GL_ARRAY_BUFFER), buffer, "the first context kept its binding");
}

/// The host EGL context behind an EAGLContext (it is made current to find it).
fn egl_context_of(context: usize) -> usize {
    make_current(context);
    crate::fw::eagl::current_egl_context()
}

#[test]
fn freed_eagl_contexts_leave_the_gles_tracking() {
    setup();
    let release = |context: usize| crate::objc::runtime::send(context, "release", &[]);
    // A context current nowhere goes at its last release.
    let lone = eagl_context(0);
    let lone_egl = egl_context_of(lone);
    make_current(0);
    assert!(gles::context_tracked(lone_egl));
    release(lone);
    assert!(!gles::context_tracked(lone_egl), "a freed context's tracking entries go");

    // A sharegroup outlives the context that started it while a partner lives.
    let a = eagl_context(0);
    let b = eagl_context(a);
    let (a_egl, b_egl) = (egl_context_of(a), egl_context_of(b));
    make_current(a);
    let (buffer, _) = filled_buffer();
    assert_ne!(call("glMapBufferRange", [GL_ARRAY_BUFFER, 0, 8, GL_MAP_WRITE_BIT], &[]).0, 0, "the buffer mapped");
    make_current(0);
    release(a);
    make_current(b);
    call("glBindBuffer", [GL_ARRAY_BUFFER, buffer, 0, 0], &[]);
    assert_eq!(call("glUnmapBuffer", [GL_ARRAY_BUFFER, 0, 0, 0], &[]).0, 1, "the partner still finds the group's mapping");
    make_current(0);
    release(b);
    assert!(!gles::context_tracked(a_egl) && !gles::context_tracked(b_egl), "the last member takes the group with it");

    // As on Apple, the current context is retained: released while current,
    // it stays usable and goes once another context (or none) is made current.
    let current = eagl_context(0);
    let current_egl = egl_context_of(current);
    release(current);
    assert!(gles::context_tracked(current_egl), "a current context stays");
    let (buffer, _) = filled_buffer();
    assert_eq!(gles::bound_buffer(GL_ARRAY_BUFFER), buffer, "and keeps working");
    make_current(0);
    assert!(!gles::context_tracked(current_egl), "then goes once let go");
}

#[test]
fn game_retains_autoreleases_and_releases_reach_the_eagl_context_count() {
    use crate::guest32::objc::bridge::Bridge;
    use crate::guest32::objc::functions::{call_function, Ctx};
    use crate::guest32::objc::testing::Fake;
    setup();
    let g = Fake::new();
    let bridge: &'static std::sync::Mutex<Bridge> =
        Box::leak(Box::new(std::sync::Mutex::new(Bridge::new(&guest32_image::ObjcMetadata32::default()).unwrap())));
    let ctx = Ctx { bridge, g };
    let call = |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&ctx, name, &w).unwrap() as u32
    };
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(g, name);
    // [[EAGLContext alloc] initWithAPI:kEAGLRenderingAPIOpenGLES2] from game code.
    let class = bridge.lock().unwrap().framework_class(g, "EAGLContext").0;
    let context = call("objc_msgSend", &[call("objc_msgSend", &[class, sel("alloc")]), sel("initWithAPI:"), 2]);
    let host = bridge.lock().unwrap().host_object(context).expect("a host EAGLContext");
    let count = || crate::fw::counted::count(host);
    assert_eq!(count(), Some(1));

    // ARC, a retain message and CFRetain (the host function the guest's reaches) all count.
    call("objc_retain", &[context]);
    call("objc_msgSend", &[context, sel("retain")]);
    let (cf_retain, _) = crate::symbols::lookup("_CFRetain").expect("CFRetain");
    let cf_retain: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(cf_retain) };
    cf_retain(host);
    assert_eq!(count(), Some(4));
    // Autoreleases wait for their pool, then each is one release.
    let pool = call("objc_autoreleasePoolPush", &[]);
    call("objc_autorelease", &[context]);
    call("objc_msgSend", &[context, sel("autorelease")]);
    call("objc_autoreleaseReturnValue", &[context]);
    assert_eq!(count(), Some(4), "autoreleased, not yet released");
    call("objc_autoreleasePoolPop", &[pool]);
    assert_eq!(count(), Some(1));
    // Strong stores and retained associated objects hold it until let go.
    let slot = g.words(&[0]);
    call("objc_storeStrong", &[slot, context]);
    let holder = g.words(&[0]);
    call("objc_setAssociatedObject", &[holder, 0x77, context, 0x301]);
    assert_eq!(count(), Some(3));
    call("objc_storeStrong", &[slot, 0]);
    call("objc_removeAssociatedObjects", &[holder]);
    assert_eq!(count(), Some(1), "removed associated objects are released");
    call("objc_setAssociatedObject", &[holder, 0x78, context, 1]);
    call("objc_setAssociatedObject", &[holder, 0x78, 0, 0]);
    assert_eq!(count(), Some(1), "a value set back to nil is released by its own policy");

    // Being current holds it: the game's last release leaves it alive and working.
    assert_eq!(call("objc_msgSend", &[class, sel("setCurrentContext:"), context]) & 0xff, 1);
    let egl = crate::fw::eagl::current_egl_context();
    call("objc_msgSend", &[context, sel("release")]);
    assert_eq!(count(), Some(1), "the current context survives the game's release");
    assert!(gles::context_tracked(egl));
    assert_eq!(call("objc_msgSend", &[class, sel("currentContext")]), context);
    call("objc_msgSend", &[class, sel("setCurrentContext:"), 0]);
    assert_eq!(count(), None, "then goes once it is no longer current");
    assert!(!gles::context_tracked(egl), "its EGL context went with it");
}

#[test]
fn indexed_gl_queries_reach_the_guest_registry() {
    setup();
    for name in ["glGetIntegeri_v", "glGetInteger64i_v", "glGetBooleani_v"] {
        assert!(ios32::provides_function(name), "{name} is registered");
    }
}
