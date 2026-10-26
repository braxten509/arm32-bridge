//! The bridge with a made-up guest (memory buffer + Rust closures standing in
//! for guest code) and real host classes in the 64-bit runtime.

use super::bridge::*;
use super::convert::{HostFrame, HostRet};
use super::mirror;
use super::testing::Fake;
use crate::objc::runtime::{self, Class, Id, Sel};
use guest32_image::{ClassRo32, ClassTarget, Method32, ObjcCategory32, ObjcClass32, ObjcMetadata32};
use std::sync::Mutex;

// ----- host classes for the test -----

extern "C" fn host_alloc(class: Class, _: Sel) -> Id {
    runtime::alloc_instance(class)
}

static HOST_ALPHA: Mutex<Vec<(Id, f64)>> = Mutex::new(Vec::new());

extern "C" fn host_set_alpha(this: Id, _: Sel, alpha: f64) {
    HOST_ALPHA.lock().unwrap().push((this, alpha));
}

#[repr(C)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

extern "C" fn host_frame(_: Id, _: Sel) -> Rect {
    Rect { x: 1.0, y: 2.0, w: 30.0, h: 40.0 }
}

extern "C" fn host_object_at_index(this: Id, _: Sel, index: usize) -> Id {
    if index == 0 {
        this
    } else {
        0
    }
}

static HOST_BYTES: [u8; 4] = [1, 2, 3, 4];

extern "C" fn host_bytes(_: Id, _: Sel) -> *const u8 {
    HOST_BYTES.as_ptr()
}

extern "C" fn host_length(_: Id, _: Sel) -> usize {
    HOST_BYTES.len()
}

fn host_class(name: &str) -> Class {
    let mut rt = runtime::lock();
    let class = rt.define_class(name, 0, 16);
    let meta = rt.classes[&class].partner;
    let (alloc, alpha, frame, at) =
        (rt.sel("alloc"), rt.sel("setAlpha:"), rt.sel("frame"), rt.sel("objectAtIndex:"));
    rt.add_method(meta, alloc, host_alloc as usize);
    rt.add_method(class, alpha, host_set_alpha as usize);
    rt.add_method(class, frame, host_frame as usize);
    rt.add_method(class, at, host_object_at_index as usize);
    let (bytes, length) = (rt.sel("bytes"), rt.sel("length"));
    rt.add_method(class, bytes, host_bytes as usize);
    rt.add_method(class, length, host_length as usize);
    class
}

fn method(name: &str, types: &str, imp: u32) -> Method32 {
    Method32 { name: name.into(), types: types.into(), imp }
}

/// Game class `G32Sprite : G32HostView` with -setAlpha: (calls super) and
/// -tick:(int) returning an int; metaclass with +sprite.
fn game(fake: &Fake) -> (Mutex<Bridge>, u32, u32) {
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Sprite".into(),
            superclass: ClassTarget::External("G32HostView".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("G32HostView".into()),
            meta_superclass: ClassTarget::External("G32HostView".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                methods: vec![method("setAlpha:", "v12@0:4f8", 0x2001), method("tick:", "i12@0:4i8", 0x2011)],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, methods: vec![method("sprite", "@8@0:4", 0x2021)], ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge = Bridge::new(&md).unwrap();
    let bridge = Mutex::new(bridge);
    install(&bridge, fake, &md).unwrap();
    (bridge, class, meta)
}

fn send(bridge: &Mutex<Bridge>, fake: &Fake, words: &[u32]) -> Result<u64, String> {
    let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
    send_from_guest(bridge, fake, SendKind::Normal, &w).map(|r| u64::from(r.r0) | u64::from(r.r1) << 32)
}

#[test]
fn guest_messages_reach_game_code_host_code_and_super() {
    let host_view = host_class("G32HostView");
    let fake = Fake::new();
    let (bridge, class, meta) = game(fake);
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);

    // +[G32Sprite alloc] goes to the host's alloc with the mirror class; the
    // guest gets a guest instance of the game class (isa = the game class).
    let object = send(&bridge, fake, &[class, sel("alloc")]).unwrap() as u32;
    assert_eq!(fake.u32_at(object), class);
    let host = bridge.lock().unwrap().host_object(object).unwrap();
    let mirror_class = runtime::lock().class_of(host);
    assert_eq!(runtime::lock().classes[&mirror_class].superclass, host_view);
    // Identity is stable both ways.
    assert_eq!(bridge.lock().unwrap().guest_object(fake, host).unwrap(), object);

    // A game method runs as guest code with the guest's words.
    fake.code(0x2011, |w| u64::from(w[2] * 3));
    assert_eq!(send(&bridge, fake, &[object, sel("tick:"), 14]).unwrap(), 42);

    // -setAlpha: is the game's; it calls [super setAlpha:] which must reach
    // the host's implementation (not the mirror's trampoline again).
    let bridge_ref: &'static Mutex<Bridge> = Box::leak(Box::new(bridge));
    let super_struct = fake.alloc_permanent(8);
    fake.code(0x2001, move |w| {
        fake.write(super_struct, &[w[0].to_le_bytes(), class.to_le_bytes()].concat());
        let words = [super_struct, w[1], w[2]];
        let get = |i: u32| words[i as usize];
        send_from_guest(bridge_ref, fake, SendKind::Super2, &get).unwrap();
        0
    });
    let alpha = bridge_ref.lock().unwrap().guest_selector(fake, "setAlpha:");
    send(bridge_ref, fake, &[object, alpha, 0.5f32.to_bits()]).unwrap();
    assert_eq!(HOST_ALPHA.lock().unwrap().iter().filter(|e| e.0 == host).collect::<Vec<_>>(), [&(host, 0.5)]);

    // A framework selector the game does not implement goes to the host with
    // converted types: -frame (CGRect stret) and -objectAtIndex: (id result).
    let out = fake.alloc_permanent(16);
    let frame_sel = bridge_ref.lock().unwrap().guest_selector(fake, "frame");
    let w = [out, object, frame_sel];
    let get = |i: u32| w[i as usize];
    send_from_guest(bridge_ref, fake, SendKind::Stret, &get).unwrap();
    let floats: Vec<f32> = fake.read(out, 16).chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(floats, [1.0, 2.0, 30.0, 40.0]);
    let at = bridge_ref.lock().unwrap().guest_selector(fake, "objectAtIndex:");
    assert_eq!(send(bridge_ref, fake, &[object, at, 0]).unwrap() as u32, object);
    // -bytes points into host memory: the guest gets a copy in guest memory.
    let bytes = bridge_ref.lock().unwrap().guest_selector(fake, "bytes");
    let copy = send(bridge_ref, fake, &[object, bytes]).unwrap() as u32;
    assert_eq!(fake.read(copy, 4), HOST_BYTES);

    // A class method of the game runs on the guest class object.
    fake.code(0x2021, move |w| u64::from(w[0]));
    let sprite = bridge_ref.lock().unwrap().guest_selector(fake, "sprite");
    assert_eq!(send(bridge_ref, fake, &[class, sprite]).unwrap() as u32, class);
    let _ = meta;

    // A selector with no encoding and no game method fails loudly with its name.
    let unknown = bridge_ref.lock().unwrap().guest_selector(fake, "frobnicate:");
    let error = send(bridge_ref, fake, &[object, unknown, 1]).unwrap_err();
    assert!(error.contains("frobnicate:"), "{error}");
    // Keeping going (always on the phone): that one message is skipped, the
    // game gets 0 back, and the log and the missing list name it.
    let mut watch = crate::report::LineWatch::now();
    let skipped = super::functions::skip_missing(send_from_guest(bridge_ref, fake, SendKind::Normal, &|i| [object, unknown, 1].get(i as usize).copied().unwrap_or(0)), true);
    assert_eq!(skipped.map(|r| (r.r0, r.r1)), Ok((0, 0)));
    assert!(watch.wait_for("ERROR: missing method", std::time::Duration::ZERO), "an error line in the log");
    assert!(runtime::missing_methods().iter().any(|(name, _)| name.contains("frobnicate:")), "in the missing list");
    // A failure that isn't a missing method still stops the call.
    assert!(super::functions::skip_missing(Err("guest object 0x10 is not known to the bridge".into()), true).is_err());

    // The host messages the game object: the trampoline converts and runs guest code.
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge_ref, fake, frame).unwrap();
    }));
    let tick = runtime::sel("tick:");
    let imp = runtime::imp_for(host, tick).unwrap();
    let mut args = super::convert::HostArgs::default();
    args.x[..3].copy_from_slice(&[host as u64, tick as u64, 5]);
    let ret: HostRet = mirror::call_host(imp, &args);
    assert_eq!(ret.x[0] as u32, 15);
}

/// A game class in made-up metadata: its class and metaclass structures
/// (isa words written) at fresh guest addresses.
fn class32(fake: &Fake, name: &str, superclass: ClassTarget, root: &str, methods: Vec<Method32>) -> ObjcClass32 {
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let meta_superclass = match &superclass {
        ClassTarget::Local(parent) => ClassTarget::Local(fake.u32_at(*parent)),
        other => other.clone(),
    };
    ObjcClass32 {
        address: class,
        name: name.into(),
        superclass,
        meta_address: meta,
        meta_isa: ClassTarget::External(root.into()),
        meta_superclass,
        instance: ClassRo32 { instance_start: 8, instance_size: 16, methods, ..Default::default() },
        meta: ClassRo32 { flags: 1, ..Default::default() },
    }
}

/// A game class whose superclass grew after it was compiled (ivars in the
/// superclass's @implementation) gets its ivars moved past the superclass,
/// keeping their alignment, like Apple's runtime does when it realizes classes.
#[test]
fn subclass_ivars_slide_past_a_larger_game_superclass() {
    host_class("G32HostSlideRoot");
    let fake = Fake::new();
    let ivar = |fake: &Fake, offset: u32, alignment: u32, size: u32| guest32_image::Ivar32 {
        offset_address: fake.words(&[offset]),
        offset,
        name: format!("_at{offset}"),
        type_encoding: "@".into(),
        alignment,
        size,
    };
    let mut base = class32(fake, "G32SlideBase", ClassTarget::External("G32HostSlideRoot".into()), "G32HostSlideRoot", vec![]);
    base.instance = ClassRo32 { instance_start: 4, instance_size: 16, ivars: vec![ivar(fake, 4, 2, 4), ivar(fake, 8, 2, 4), ivar(fake, 12, 2, 4)], ..Default::default() };
    // Compiled seeing only the root: one ivar at 4.
    let mut sub = class32(fake, "G32SlideSub", ClassTarget::Local(base.address), "G32HostSlideRoot", vec![]);
    sub.instance = ClassRo32 { instance_start: 4, instance_size: 8, ivars: vec![ivar(fake, 4, 2, 4)], ..Default::default() };
    // Below it, a double (8-aligned) compiled at 8: the slide keeps 8-alignment.
    let mut leaf = class32(fake, "G32SlideLeaf", ClassTarget::Local(sub.address), "G32HostSlideRoot", vec![]);
    leaf.instance = ClassRo32 { instance_start: 8, instance_size: 16, ivars: vec![ivar(fake, 8, 3, 8)], ..Default::default() };
    let (base_ivars, sub_ivar, leaf_ivar) = (base.instance.ivars.clone(), sub.instance.ivars[0].offset_address, leaf.instance.ivars[0].offset_address);
    let (sub_class, leaf_class) = (sub.address, leaf.address);
    let md = ObjcMetadata32 { classes: vec![leaf, sub, base], ..Default::default() };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    assert_eq!(base_ivars.iter().map(|i| fake.u32_at(i.offset_address)).collect::<Vec<_>>(), [4, 8, 12], "the superclass keeps its layout");
    assert_eq!(fake.u32_at(sub_ivar), 16, "past the superclass's 16 bytes");
    assert_eq!(fake.u32_at(leaf_ivar), 24, "20 rounded up to the double's 8");
    let b = bridge.lock().unwrap();
    assert_eq!(b.classes.class(sub_class).map(|c| (c.instance_start, c.instance_size)), Some((16, 20)));
    assert_eq!(b.classes.class(leaf_class).map(|c| (c.instance_start, c.instance_size, c.ivars[0].offset)), Some((24, 32, 24)));
}

#[test]
fn super_calls_pass_through_intermediate_game_classes() {
    host_class("G32HostPanel");
    let fake = Fake::new();
    // G32Leaf : G32Middle : G32HostPanel; only G32Leaf has -setAlpha:, which
    // calls [super setAlpha:]. That must reach the host's method once.
    let middle = class32(fake, "G32Middle", ClassTarget::External("G32HostPanel".into()), "G32HostPanel", vec![]);
    let leaf = class32(fake, "G32Leaf", ClassTarget::Local(middle.address), "G32HostPanel", vec![method("setAlpha:", "v12@0:4f8", 0x2101)]);
    let leaf_class = leaf.address;
    let md = ObjcMetadata32 { classes: vec![middle, leaf], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let entries: &'static Mutex<u32> = Box::leak(Box::new(Mutex::new(0)));
    let super_struct = fake.alloc_permanent(8);
    fake.code(0x2101, move |w| {
        *entries.lock().unwrap() += 1;
        if *entries.lock().unwrap() > 1 {
            return 0; // re-entered: the super call came back here
        }
        fake.write(super_struct, &[w[0].to_le_bytes(), leaf_class.to_le_bytes()].concat());
        let words = [super_struct, w[1], w[2]];
        send_from_guest(bridge, fake, SendKind::Super2, &|i| words[i as usize]).unwrap();
        0
    });
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let object = send(bridge, fake, &[leaf_class, sel("alloc")]).unwrap() as u32;
    let host = bridge.lock().unwrap().host_object(object).unwrap();
    send(bridge, fake, &[object, sel("setAlpha:"), 0.25f32.to_bits()]).unwrap();
    assert_eq!(*entries.lock().unwrap(), 1);
    assert!(HOST_ALPHA.lock().unwrap().contains(&(host, 0.25)));
}

#[test]
fn a_repeated_game_message_uses_the_remembered_method_until_the_game_adds_one() {
    // SpellCraft's fights stalled ~70 ms twice a second: every message
    // looked its method up again by name. The answer is now remembered per
    // (class, selector), and forgotten when the game adds a method.
    host_class("G32HostCachePanel");
    let fake = Fake::new();
    let base = class32(fake, "G32CacheBase", ClassTarget::External("G32HostCachePanel".into()), "G32HostCachePanel", vec![method("spin", "i8@0:4", 0x2201)]);
    let leaf = class32(fake, "G32CacheLeaf", ClassTarget::Local(base.address), "G32HostCachePanel", vec![]);
    let leaf_class = leaf.address;
    let md = ObjcMetadata32 { classes: vec![base, leaf], ..Default::default() };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    fake.code(0x2201, |_| 1);
    fake.code(0x2211, |_| 2);
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let object = send(&bridge, fake, &[leaf_class, sel("alloc")]).unwrap() as u32;
    let spin = sel("spin");
    assert_eq!(send(&bridge, fake, &[object, spin]).unwrap(), 1);
    assert!(bridge.lock().unwrap().classes.cached_send(leaf_class, spin).is_some(), "the answer is remembered");
    assert_eq!(send(&bridge, fake, &[object, spin]).unwrap(), 1);
    // class_addMethod: the leaf's own -spin answers from now on.
    assert!(bridge.lock().unwrap().classes.add_method(leaf_class, "spin", 0x2211, "i8@0:4"));
    assert_eq!(send(&bridge, fake, &[object, spin]).unwrap(), 2);
}

#[test]
fn bytes_asks_a_game_override_of_length_without_holding_the_bridge() {
    host_class("G32HostBlobBase");
    let fake = Fake::new();
    // G32Blob : G32HostBlobBase overrides -length in guest code (2 bytes).
    let blob = class32(fake, "G32Blob", ClassTarget::External("G32HostBlobBase".into()), "G32HostBlobBase", vec![method("length", "I8@0:4", 0x2201)]);
    let class = blob.address;
    let md = ObjcMetadata32 { classes: vec![blob], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    fake.code(0x2201, |_| 2);
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let object = send(bridge, fake, &[class, sel("alloc")]).unwrap() as u32;
    let bytes = sel("bytes");
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(send(bridge, fake, &[object, bytes])).unwrap());
    let copy = result.recv_timeout(std::time::Duration::from_secs(10)).expect("-bytes deadlocked on the bridge lock").unwrap() as u32;
    assert_eq!(fake.read(copy, 2), &HOST_BYTES[..2]);
}

#[test]
fn load_runs_parents_first_then_categories_and_initialize_runs_once_before_first_message() {
    host_class("G32HostRoot");
    let fake = Fake::new();
    let mut base = class32(fake, "G32Base", ClassTarget::External("G32HostRoot".into()), "G32HostRoot", vec![]);
    base.meta.methods = vec![method("load", "v8@0:4", 0x2311), method("initialize", "v8@0:4", 0x2301)];
    let mut sub = class32(fake, "G32Sub", ClassTarget::Local(base.address), "G32HostRoot", vec![]);
    sub.meta.methods = vec![method("load", "v8@0:4", 0x2321)];
    let (base_class, sub_class) = (base.address, sub.address);
    let category = ObjcCategory32 {
        address: fake.alloc_permanent(4),
        name: "Extras".into(),
        class: ClassTarget::Local(sub_class),
        instance_methods: vec![],
        class_methods: vec![method("load", "v8@0:4", 0x2331)],
        protocols: vec![],
        instance_properties: vec![],
    };
    // The subclass comes first in the image; its superclass still loads first.
    let md = ObjcMetadata32 { classes: vec![sub, base], categories: vec![category], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let calls: &'static Mutex<Vec<(u32, u32)>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    for imp in [0x2301, 0x2311, 0x2321, 0x2331] {
        fake.code(imp, move |w| {
            calls.lock().unwrap().push((imp, w[0]));
            0
        });
    }
    run_loads(bridge, fake, &md).unwrap();
    assert_eq!(*calls.lock().unwrap(), [(0x2311, base_class), (0x2321, sub_class), (0x2331, sub_class)]);
    calls.lock().unwrap().clear();
    // The first message to G32Sub initializes G32Base, then G32Sub (inheriting
    // +initialize, as on Apple's runtime); later messages don't.
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let object = send(bridge, fake, &[sub_class, sel("alloc")]).unwrap() as u32;
    let _ = object;
    send(bridge, fake, &[sub_class, sel("alloc")]).unwrap();
    send(bridge, fake, &[base_class, sel("alloc")]).unwrap();
    assert_eq!(*calls.lock().unwrap(), [(0x2301, base_class), (0x2301, sub_class)]);
}

#[test]
fn other_threads_wait_for_initialize_and_the_owner_may_reenter() {
    use std::sync::atomic::{AtomicBool, Ordering};
    host_class("G32HostRootI");
    let fake = Fake::new();
    let mut base = class32(fake, "G32InitBase", ClassTarget::External("G32HostRootI".into()), "G32HostRootI", vec![]);
    base.meta.methods = vec![method("initialize", "v8@0:4", 0x2601), method("ping:", "v12@0:4i8", 0x2611)];
    let class = base.address;
    let md = ObjcMetadata32 { classes: vec![base], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let ping = bridge.lock().unwrap().guest_selector(fake, "ping:");
    let log: &'static Mutex<Vec<u32>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    let (started, release): (&'static AtomicBool, &'static AtomicBool) =
        (Box::leak(Box::new(AtomicBool::new(false))), Box::leak(Box::new(AtomicBool::new(false))));
    fake.code(0x2611, move |w| {
        log.lock().unwrap().push(w[2]);
        0
    });
    // +initialize messages its own class (the owner re-enters), then waits.
    fake.code(0x2601, move |w| {
        log.lock().unwrap().push(100);
        send(bridge, fake, &[w[0], ping, 1]).unwrap();
        started.store(true, Ordering::SeqCst);
        while !release.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        log.lock().unwrap().push(101);
        0
    });
    let a = std::thread::spawn(move || send(bridge, fake, &[class, ping, 0]).unwrap());
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let b = std::thread::spawn(move || send(bridge, fake, &[class, ping, 2]).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!log.lock().unwrap().contains(&2), "thread B ran a method before +initialize finished");
    release.store(true, Ordering::SeqCst);
    a.join().unwrap();
    b.join().unwrap();
    let log = log.lock().unwrap().clone();
    let end = log.iter().position(|&e| e == 101).unwrap();
    assert_eq!(&log[..end + 1], [100, 1, 101]);
    assert!(log[end + 1..].contains(&2) && log[end + 1..].contains(&0));
}

#[test]
fn a_framework_category_initialize_runs_for_the_class_and_its_subclasses() {
    let root = host_class("G32HostRootC");
    let leaf = runtime::lock().define_class("G32HostLeafC", root, 16);
    let fake = Fake::new();
    // A game category on G32HostRootC with +initialize; G32Kid : G32HostRootC has none.
    let category = ObjcCategory32 {
        address: fake.alloc_permanent(4),
        name: "Setup".into(),
        class: ClassTarget::External("G32HostRootC".into()),
        instance_methods: vec![],
        class_methods: vec![method("initialize", "v8@0:4", 0x2701)],
        protocols: vec![],
        instance_properties: vec![],
    };
    let kid = class32(fake, "G32Kid", ClassTarget::External("G32HostRootC".into()), "G32HostRootC", vec![]);
    let kid_class = kid.address;
    let md = ObjcMetadata32 { classes: vec![kid], categories: vec![category], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let calls: &'static Mutex<Vec<u32>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    fake.code(0x2701, move |w| {
        calls.lock().unwrap().push(w[0]);
        0
    });
    let root_guest = bridge.lock().unwrap().framework_class(fake, "G32HostRootC").0;
    // The game subclass's first message: the framework class, then the subclass
    // (inheriting the category's +initialize).
    let alloc = bridge.lock().unwrap().guest_selector(fake, "alloc");
    send(bridge, fake, &[kid_class, alloc]).unwrap();
    send(bridge, fake, &[kid_class, alloc]).unwrap();
    assert_eq!(*calls.lock().unwrap(), [root_guest, kid_class]);
    // The host runtime's own +initialize of a host subclass reaches it once.
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let initialize = runtime::sel("initialize");
    let imp = runtime::imp_for(leaf, initialize).expect("the category's +initialize is on the host class");
    let mut args = super::convert::HostArgs::default();
    args.x[..2].copy_from_slice(&[leaf as u64, initialize as u64]);
    mirror::call_host(imp, &args);
    mirror::call_host(imp, &args);
    let leaf_guest = bridge.lock().unwrap().framework_class(fake, "G32HostLeafC").0;
    assert_eq!(*calls.lock().unwrap(), [root_guest, kid_class, leaf_guest]);
}

extern "C" fn host_label(_: Id, _: Sel) -> usize {
    7
}

#[test]
fn framework_categories_reach_subclasses_from_guest_and_host() {
    let parent = host_class("G32HostObj");
    let sub = runtime::lock().define_class("G32HostStr", parent, 16);
    // The subclass has its own -label, which beats the parent's category method.
    {
        let mut rt = runtime::lock();
        let label = rt.sel("label");
        rt.add_method(sub, label, host_label as usize);
    }
    let fake = Fake::new();
    // A game category on G32HostObj adds -twice:; G32HostStr inherits it.
    let category = ObjcCategory32 {
        address: fake.alloc_permanent(4),
        name: "Twice".into(),
        class: ClassTarget::External("G32HostObj".into()),
        instance_methods: vec![method("twice:", "i12@0:4i8", 0x2401), method("label", "i8@0:4", 0x2402)],
        class_methods: vec![],
        protocols: vec![],
        instance_properties: vec![],
    };
    let md = ObjcMetadata32 { categories: vec![category], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    fake.code(0x2401, |w| u64::from(w[2] * 2));
    let host = runtime::alloc_instance(sub);
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    // From the guest: found on the parent class.
    let twice = bridge.lock().unwrap().guest_selector(fake, "twice:");
    assert_eq!(send(bridge, fake, &[proxy, twice, 21]).unwrap(), 42);
    fake.code(0x2402, |_| 99);
    let label = bridge.lock().unwrap().guest_selector(fake, "label");
    assert_eq!(send(bridge, fake, &[proxy, label]).unwrap(), 7, "the subclass's own method is nearer");
    let parent_proxy = bridge.lock().unwrap().guest_object(fake, runtime::alloc_instance(parent)).unwrap();
    assert_eq!(send(bridge, fake, &[parent_proxy, label]).unwrap(), 99);
    // From the host: the host class answers through the trampoline.
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let sel = runtime::sel("twice:");
    let imp = runtime::imp_for(host, sel).expect("the host class has the category method");
    let mut args = super::convert::HostArgs::default();
    args.x[..3].copy_from_slice(&[host as u64, sel as u64, 8]);
    assert_eq!(mirror::call_host(imp, &args).x[0] as u32, 16);
}

static HOST_CG: Mutex<Vec<Id>> = Mutex::new(Vec::new());

extern "C" fn host_cg_image(this: Id, _: Sel) -> Id {
    // A CGImage is an object of this layer, kept beside its UIImage.
    unsafe { *((this + 8) as *const Id) }
}

extern "C" fn host_init_with_cg_image(this: Id, _: Sel, image: Id) -> Id {
    HOST_CG.lock().unwrap().push(image);
    this
}

#[test]
fn cg_objects_from_getters_cross_as_their_proxies() {
    let class = host_class("G32HostImage");
    {
        let mut rt = runtime::lock();
        let (get, init) = (rt.sel("CGImage"), rt.sel("initWithCGImage:"));
        rt.add_method(class, get, host_cg_image as usize);
        rt.add_method(class, init, host_init_with_cg_image as usize);
    }
    let (image, cg) = (runtime::alloc_instance(class), runtime::alloc_instance(host_class("G32HostCGImage")));
    unsafe { *((image + 8) as *mut Id) = cg };
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let proxy = bridge.lock().unwrap().guest_object(fake, image).unwrap();
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    // -CGImage returns ^{CGImage=}: a host pointer outside guest memory.
    let guest_cg = send(&bridge, fake, &[proxy, sel("CGImage")]).unwrap() as u32;
    assert_eq!(guest_cg, bridge.lock().unwrap().guest_object(fake, cg).unwrap(), "the CG object's own proxy");
    send(&bridge, fake, &[proxy, sel("initWithCGImage:"), guest_cg]).unwrap();
    assert_eq!(*HOST_CG.lock().unwrap(), [cg]);
}

static HOST_VOLUME: Mutex<Vec<(Id, f32)>> = Mutex::new(Vec::new());

extern "C" fn host_set_volume(this: Id, _: Sel, volume: f32) {
    HOST_VOLUME.lock().unwrap().push((this, volume));
}

/// Host class `G32Player` with +alloc and -setVolume: taking a C float.
fn host_volume_class() -> Class {
    let mut rt = runtime::lock();
    let class = rt.define_class("G32Player", 0, 16);
    let meta = rt.classes[&class].partner;
    let (alloc, volume) = (rt.sel("alloc"), rt.sel("setVolume:"));
    rt.add_method(meta, alloc, host_alloc as usize);
    rt.add_method(class, volume, host_set_volume as usize);
    class
}

#[test]
fn a_float_host_property_reaches_the_host_as_a_float() {
    // The game's CDLongAudioSource has its own -setVolume: (v12@0:4f8) and
    // forwards it to AVAudioPlayer's, whose volume is a C float on the
    // iPhone. Every message is looked up by selector name, so the player's
    // -setVolume: must learn from the table that the value is a float, not
    // from the game's method types: sent as a CGFloat (double), the player
    // reads half of it and gets 0, and the music stays silent.
    let player = host_volume_class();
    let fake = Fake::new();
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32AudioSource".into(),
            superclass: ClassTarget::External("NSObject".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("NSObject".into()),
            meta_superclass: ClassTarget::External("NSObject".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                methods: vec![method("setVolume:", "v12@0:4f8", 0x3001)],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    let host = runtime::alloc_instance(player);
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let volume = bridge.lock().unwrap().guest_selector(fake, "setVolume:");
    send(&bridge, fake, &[proxy, volume, 1.0f32.to_bits()]).unwrap();
    assert_eq!(HOST_VOLUME.lock().unwrap().clone(), vec![(host, 1.0f32)]);
}

#[test]
fn game_protocols_cross_the_bridge_and_game_classes_conform_to_them() {
    // SpellCraft asks -conformsToProtocol: with its own protocols: each is a
    // protocol_t in the game's image (isa 0 there), which the host's
    // NSObject reads in its own layout, and the class's host mirror must
    // list the protocols the game class adopts.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let (drawing, base, other) = (fake.alloc_permanent(32), fake.alloc_permanent(32), fake.alloc_permanent(32));
    let protocol = |address: u32, name: &str, protocols: Vec<u32>| guest32_image::ObjcProtocol32 {
        address,
        name: name.into(),
        protocols,
        instance_methods: vec![method("draw", "v8@0:4", 0)],
        class_methods: vec![],
        optional_instance_methods: vec![],
        optional_class_methods: vec![],
        properties: vec![],
    };
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Shape".into(),
            superclass: ClassTarget::External("NSObject".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("NSObject".into()),
            meta_superclass: ClassTarget::External("NSObject".into()),
            instance: ClassRo32 { instance_start: 4, instance_size: 4, protocols: vec![drawing], ..Default::default() },
            meta: ClassRo32 { flags: 1, protocols: vec![drawing], ..Default::default() },
        }],
        protocols_by_address: [
            (drawing, protocol(drawing, "G32Drawing", vec![base])),
            (base, protocol(base, "G32Base", vec![])),
            (other, protocol(other, "G32Other", vec![])),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    let host = bridge.lock().unwrap().host_object_for(fake, drawing).unwrap();
    assert_eq!(unsafe { runtime::cstr(*((host + 8) as *const usize)) }, "G32Drawing");
    assert_eq!(bridge.lock().unwrap().guest_object(fake, host).unwrap(), drawing, "the same protocol comes back");
    assert_eq!(runtime::protocol_named("G32Drawing"), host, "objc_getProtocol finds it");
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let object = send(&bridge, fake, &[class, sel("alloc")]).unwrap() as u32;
    let conforms = sel("conformsToProtocol:");
    assert_eq!(send(&bridge, fake, &[object, conforms, drawing]).unwrap() & 0xff, 1);
    assert_eq!(send(&bridge, fake, &[object, conforms, base]).unwrap() & 0xff, 1, "a protocol it includes");
    assert_eq!(send(&bridge, fake, &[object, conforms, other]).unwrap() & 0xff, 0);
    assert_eq!(send(&bridge, fake, &[class, conforms, drawing]).unwrap() & 0xff, 1, "the class too");
}

/// An `id *` out-parameter the host leaves alone keeps the game's value (a
/// failed NSScanner scan leaves *into as it was); one it sets is the new
/// value. SpellCraft scans each map cell, then tries a second scan into the
/// same variable: the cell was wiped to nil, so the dungeon had no rooms.
#[test]
fn an_out_object_the_host_does_not_set_keeps_the_games_value() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let guest = |host: Id| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let text = |guest: u32| crate::fw::string::get(bridge.lock().unwrap().host_object(guest).unwrap());
    let s = crate::fw::string::make;
    let scanner = guest(runtime::send(runtime::class_named("NSScanner"), "scannerWithString:", &[s("0,#")]));
    let slot = fake.alloc_permanent(4);
    let scan = |selector: &str, wanted: &str| {
        let sel = bridge.lock().unwrap().guest_selector(fake, selector);
        send(&bridge, fake, &[scanner, sel, guest(s(wanted)), slot]).unwrap() & 0xff
    };
    assert_eq!(scan("scanUpToString:intoString:", ","), 1);
    assert_eq!(text(fake.u32_at(slot)), "0");
    assert_eq!(scan("scanString:intoString:", "#"), 0, "the next character is a comma");
    assert_eq!(text(fake.u32_at(slot)), "0", "a failed scan leaves the variable alone");
}

static KVO_HEARD: Mutex<Vec<(String, Id, Id)>> = Mutex::new(Vec::new());

extern "C" fn host_observe(_this: Id, _: Sel, path: Id, object: Id, change: Id, _context: usize) {
    let new = crate::fw::collections::dictionary_get(change, crate::fw::string::make("new"));
    KVO_HEARD.lock().unwrap().push((crate::fw::string::get(path), object, new));
}

/// A game's own setter sent to an object something observes tells the
/// observer, as Apple's automatic KVO does. SpellCraft's wand learns of its
/// new spell this way; without it the wand never charged.
#[test]
fn a_game_setter_on_an_observed_game_object_tells_the_observer() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let watcher_class = {
        let mut rt = runtime::lock();
        let root = rt.by_name["NSObject"];
        let class = rt.define_class("G32KvoWatcher", root, 8);
        let observe = rt.sel("observeValueForKeyPath:ofObject:change:context:");
        rt.add_method(class, observe, host_observe as usize);
        class
    };
    let fake = Fake::new();
    let wand = class32(fake, "G32Wand", ClassTarget::External("NSObject".into()), "NSObject", vec![method("setSpell:", "v12@0:4@8", 0x2401), method("spell", "@8@0:4", 0x2411)]);
    let class = wand.address;
    let md = ObjcMetadata32 { classes: vec![wand], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    static SPELL: Mutex<u32> = Mutex::new(0);
    fake.code(0x2401, |w| {
        *SPELL.lock().unwrap() = w[2];
        0
    });
    fake.code(0x2411, |_| u64::from(*SPELL.lock().unwrap()));
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let guest = |host: Id| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let object = send(bridge, fake, &[class, sel("alloc")]).unwrap() as u32;
    let watcher = guest(runtime::alloc_instance(watcher_class));
    let hex = crate::fw::string::make("Hex of Chaos");
    send(bridge, fake, &[object, sel("addObserver:forKeyPath:options:context:"), watcher, guest(crate::fw::string::make("spell")), 1, 0]).unwrap();
    send(bridge, fake, &[object, sel("setSpell:"), guest(hex)]).unwrap();
    let host = bridge.lock().unwrap().host_object(object).unwrap();
    let heard = std::mem::take(&mut *KVO_HEARD.lock().unwrap());
    assert_eq!(heard, [("spell".to_string(), host, hex)]);
}

static MANUAL_KVO_HEARD: Mutex<Vec<(String, Id, Id)>> = Mutex::new(Vec::new());

extern "C" fn host_observe_manual(_this: Id, _: Sel, path: Id, object: Id, change: Id, _context: usize) {
    let new = crate::fw::collections::dictionary_get(change, crate::fw::string::make("new"));
    MANUAL_KVO_HEARD.lock().unwrap().push((crate::fw::string::get(path), object, new));
}

/// A game class may turn the automatic notice off and announce changes
/// itself (+automaticallyNotifiesObserversForKey: returning NO). The
/// bridge must ask the game's own class method: inheriting NSObject's YES
/// would announce every change a second time.
#[test]
fn a_game_that_announces_its_own_changes_is_not_announced_for_twice() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let watcher_class = {
        let mut rt = runtime::lock();
        let root = rt.by_name["NSObject"];
        let class = rt.define_class("G32ManualKvoWatcher", root, 8);
        let observe = rt.sel("observeValueForKeyPath:ofObject:change:context:");
        rt.add_method(class, observe, host_observe_manual as usize);
        class
    };
    let fake = Fake::new();
    let mut wand = class32(fake, "G32ManualWand", ClassTarget::External("NSObject".into()), "NSObject", vec![method("setSpell:", "v12@0:4@8", 0x2401), method("spell", "@8@0:4", 0x2411)]);
    wand.meta = ClassRo32 { flags: 1, methods: vec![method("automaticallyNotifiesObserversForKey:", "c12@0:4@8", 0x2421)], ..Default::default() };
    let class = wand.address;
    let md = ObjcMetadata32 { classes: vec![wand], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let guest = |host: Id| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let key = guest(crate::fw::string::make("spell"));
    let (will, did) = (sel("willChangeValueForKey:"), sel("didChangeValueForKey:"));
    static SPELL: Mutex<u32> = Mutex::new(0);
    static OVERRIDE_ASKED: Mutex<Option<Id>> = Mutex::new(None);
    fake.code(0x2401, move |w| {
        let announce = |s: u32| {
            let words = [w[0], s, key];
            let get = |i: u32| words.get(i as usize).copied().unwrap_or(0);
            send_from_guest(bridge, fake, SendKind::Normal, &get).unwrap();
        };
        announce(will);
        *SPELL.lock().unwrap() = w[2];
        announce(did);
        0
    });
    fake.code(0x2411, |_| u64::from(*SPELL.lock().unwrap()));
    // NO for the observed key; the key it was asked about is kept.
    fake.code(0x2421, move |w| {
        let asked = bridge.lock().unwrap().host_object(w[2]).unwrap_or(0);
        *OVERRIDE_ASKED.lock().unwrap() = Some(asked);
        0
    });
    let object = send(bridge, fake, &[class, sel("alloc")]).unwrap() as u32;
    let watcher = guest(runtime::alloc_instance(watcher_class));
    let hex = crate::fw::string::make("Hex of Chaos");
    send(bridge, fake, &[object, sel("addObserver:forKeyPath:options:context:"), watcher, key, 1, 0]).unwrap();
    send(bridge, fake, &[object, sel("setSpell:"), guest(hex)]).unwrap();
    let host = bridge.lock().unwrap().host_object(object).unwrap();
    let heard = std::mem::take(&mut *MANUAL_KVO_HEARD.lock().unwrap());
    assert_eq!(heard, [("spell".to_string(), host, hex)], "one notice, the game's own");
    let asked = OVERRIDE_ASKED.lock().unwrap().expect("the game's own class method was asked");
    assert_eq!(crate::fw::string::get(asked), "spell", "asked about the observed key");
    assert_eq!(send(bridge, fake, &[object, sel("spell")]).unwrap() as u32, guest(hex));
}

#[test]
fn a_va_list_of_format_arguments_reaches_the_host() {
    // -initWithFormat:arguments: from a game's logging helper: the va_list
    // points at the guest's variadic words (armv7), read by the format.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let proxy = |host: Id| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let string = proxy(runtime::send(runtime::class_named("NSString"), "alloc", &[]));
    let (format, name) = (proxy(crate::fw::string::make("%d %@ %lld")), proxy(crate::fw::string::make("wizards")));
    let va = fake.alloc_permanent(16);
    fake.write(va, &[7u32.to_le_bytes(), name.to_le_bytes(), 5u32.to_le_bytes(), 1u32.to_le_bytes()].concat());
    let sel = bridge.lock().unwrap().guest_selector(fake, "initWithFormat:arguments:");
    let made = send(&bridge, fake, &[string, sel, format, va]).unwrap() as u32;
    let host = bridge.lock().unwrap().host_object(made).unwrap();
    assert_eq!(crate::fw::string::get(host), format!("7 wizards {}", 5u64 | 1 << 32));
}

extern "C" fn host_picture_size(_this: Id, _: Sel) -> crate::fw::geometry::Size {
    crate::fw::geometry::Size { width: 3.0, height: 4.0 }
}

#[test]
fn a_host_pictures_size_is_a_cgsize_even_when_the_game_has_a_union_size() {
    // SpellCraft's engine has its own -size returning a union (_AEVector2)
    // and asks UIImage for its -size: the types come from the framework
    // table (CGSize), not from the game's union, which can't be converted.
    let picture = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32Picture", 0, 16);
        let size = rt.sel("size");
        rt.add_method(class, size, host_picture_size as usize);
        class
    };
    let fake = Fake::new();
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Texture".into(),
            superclass: ClassTarget::External("NSObject".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("NSObject".into()),
            meta_superclass: ClassTarget::External("NSObject".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                methods: vec![method("size", "(_AEVector2={?=ff}[2f])8@0:4", 0x3001)],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    let host = runtime::alloc_instance(picture);
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let size = bridge.lock().unwrap().guest_selector(fake, "size");
    let out = fake.alloc_permanent(8);
    let words = [out, proxy, size];
    send_from_guest(&bridge, fake, SendKind::Stret, &|i| words[i as usize]).unwrap();
    let floats: Vec<f32> = fake.read(out, 8).chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(floats, [3.0, 4.0]);
}

extern "C" fn host_play(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn host_is_playing(_this: Id, _: Sel) -> bool {
    true
}

#[test]
fn a_host_player_answers_play_even_when_the_game_has_a_void_play() {
    // Dungeon Plunder's music manager plays AVAudioPlayer itself and checks
    // -play's BOOL ("Couldn't play Intro.mp3" when NO). The same game holds
    // CocosDenshion's CDLongAudioSource, whose own -play returns nothing
    // (v8@0:4); taken from there, the player's YES never reached the game.
    let player = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32MusicPlayer", 0, 16);
        let meta = rt.classes[&class].partner;
        let (alloc, play, playing) = (rt.sel("alloc"), rt.sel("play"), rt.sel("isPlaying"));
        rt.add_method(meta, alloc, host_alloc as usize);
        rt.add_method(class, play, host_play as usize);
        rt.add_method(class, playing, host_is_playing as usize);
        class
    };
    let fake = Fake::new();
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32LongAudioSource".into(),
            superclass: ClassTarget::External("NSObject".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("NSObject".into()),
            meta_superclass: ClassTarget::External("NSObject".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                methods: vec![method("play", "v8@0:4", 0x3001), method("isPlaying", "v8@0:4", 0x3003)],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    let host = runtime::alloc_instance(player);
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    for name in ["play", "isPlaying"] {
        let selector = bridge.lock().unwrap().guest_selector(fake, name);
        let answer = send(&bridge, fake, &[proxy, selector]).unwrap();
        assert_eq!(answer & 0xff, 1, "-[AVAudioPlayer {name}] answers YES");
    }
}

/// CMTime, the same on both sides (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct MediaTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct MediaTimeRange {
    start: MediaTime,
    duration: MediaTime,
}

/// What the host's volume methods were given: start and end volume, then
/// the start time's value and timescale.
static HOST_MIX: Mutex<Vec<(f32, f32, i64, i32)>> = Mutex::new(Vec::new());

extern "C" fn host_set_volume_at(_this: Id, _: Sel, volume: f32, time: MediaTime) {
    HOST_MIX.lock().unwrap().push((volume, volume, time.value, time.timescale));
}

extern "C" fn host_set_ramp(_this: Id, _: Sel, start: f32, end: f32, range: MediaTimeRange) {
    HOST_MIX.lock().unwrap().push((start, end, range.start.value, range.start.timescale));
}

/// A CMTime as the guest passes it: six words.
fn time_words(value: i64, timescale: i32) -> [u32; 6] {
    [value as u32, (value >> 32) as u32, timescale as u32, 1, 0, 0]
}

#[test]
fn a_float_volume_beside_a_media_time_reaches_the_host_as_a_float() {
    // AVMutableAudioMixInputParameters' -setVolume:atTime: and
    // -setVolumeRampFromStartVolume:toEndVolume:timeRange: take C floats.
    // A game class answering the same names has only 'f' in its types, read
    // as CGFloat (a double): the host would read half of it and get 0.
    let mix = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32MixParameters", 0, 16);
        let (at, ramp) = (rt.sel("setVolume:atTime:"), rt.sel("setVolumeRampFromStartVolume:toEndVolume:timeRange:"));
        rt.add_method(class, at, host_set_volume_at as usize);
        rt.add_method(class, ramp, host_set_ramp as usize);
        class
    };
    let fake = Fake::new();
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32MixInput".into(),
            superclass: ClassTarget::External("NSObject".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("NSObject".into()),
            meta_superclass: ClassTarget::External("NSObject".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                methods: vec![
                    method("setVolume:atTime:", "v36@0:4f8{?=qiIq}12", 0x3001),
                    method("setVolumeRampFromStartVolume:toEndVolume:timeRange:", "v64@0:4f8f12{?={?=qiIq}{?=qiIq}}16", 0x3003),
                ],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge = Mutex::new(Bridge::new(&md).unwrap());
    install(&bridge, fake, &md).unwrap();
    let host = runtime::alloc_instance(mix);
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let at = bridge.lock().unwrap().guest_selector(fake, "setVolume:atTime:");
    let ramp = bridge.lock().unwrap().guest_selector(fake, "setVolumeRampFromStartVolume:toEndVolume:timeRange:");
    let mut words = vec![proxy, at, 0.5f32.to_bits()];
    words.extend(time_words(30, 10));
    send(&bridge, fake, &words).unwrap();
    let mut words = vec![proxy, ramp, 1.0f32.to_bits(), 0.25f32.to_bits()];
    words.extend(time_words(7, 2));
    words.extend(time_words(4, 2));
    send(&bridge, fake, &words).unwrap();
    assert_eq!(HOST_MIX.lock().unwrap().clone(), vec![(0.5, 0.5, 30, 10), (1.0, 0.25, 7, 2)]);
}

static HOST_POINTS: Mutex<Vec<(f64, f64)>> = Mutex::new(Vec::new());

extern "C" fn host_pick(object: Id, x: f64, y: f64) -> Id {
    HOST_POINTS.lock().unwrap().push((x, y));
    object
}

#[test]
fn c_functions_reach_host_functions_with_converted_types() {
    let host = runtime::alloc_instance(host_class("G32HostThing"));
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let proxy = bridge.lock().unwrap().guest_object(fake, host).unwrap();
    // id f(id, CGPoint): the proxy becomes the host object, the point two doubles.
    let words = [proxy, 1.5f32.to_bits(), (-2.0f32).to_bits()];
    let get = |i: u32| words[i as usize];
    let sig = super::encoding::Signature::parse("@@{CGPoint=ff}").unwrap();
    let r = call_host_function(&bridge, fake, host_pick as usize, &sig, Default::default(), &get).unwrap();
    assert_eq!((r.r0, HOST_POINTS.lock().unwrap().clone()), (proxy, vec![(1.5, -2.0)]));
}

#[test]
fn framework_classes_are_built_in_guest_memory_with_their_chain() {
    host_class("G32HostBase");
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let (class, meta) = bridge.lock().unwrap().framework_class(fake, "G32HostBase");
    // class_t { isa, superclass, cache, vtable, data } in 32-bit layout.
    assert_eq!(fake.u32_at(class), meta);
    assert_eq!(fake.u32_at(class + 4), 0);
    let ro = fake.u32_at(class + 16);
    let name = fake.u32_at(ro + 16);
    assert_eq!(fake.read(name, 12), b"G32HostBase\0");
    // A root metaclass is its own isa, and its superclass is the root class.
    assert_eq!((fake.u32_at(meta), fake.u32_at(meta + 4)), (meta, class));
    assert_eq!(fake.u32_at(fake.u32_at(meta + 16)) & 1, 1);
    // Same class every time; host class objects map to it.
    assert_eq!(bridge.lock().unwrap().framework_class(fake, "G32HostBase").0, class);
    let host = runtime::class_named("G32HostBase");
    assert_eq!(bridge.lock().unwrap().guest_object(fake, host).unwrap(), class);
    // A host instance gets a proxy whose isa is the guest class.
    let object = runtime::alloc_instance(host);
    let proxy = bridge.lock().unwrap().guest_object(fake, object).unwrap();
    assert_eq!(fake.u32_at(proxy), class);
    assert_eq!(bridge.lock().unwrap().host_object(proxy), Some(object));
    let _ = fake.text("");
}

static CXX_EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

extern "C" fn host_new(class: Class, _: Sel) -> Id {
    let object = runtime::send(class, "alloc", &[]);
    runtime::send(object, "init", &[])
}

extern "C" fn host_dealloc(_: Id, _: Sel) {
    CXX_EVENTS.lock().unwrap().push("host dealloc");
}

#[test]
fn cxx_constructors_run_after_alloc_parents_first_and_destructors_at_dealloc_children_first() {
    {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32HostCxxRoot", 0, 16);
        let meta = rt.classes[&class].partner;
        let (alloc, new, dealloc) = (rt.sel("alloc"), rt.sel("new"), rt.sel("dealloc"));
        rt.add_method(meta, alloc, host_alloc as usize);
        rt.add_method(meta, new, host_new as usize);
        rt.add_method(class, dealloc, host_dealloc as usize);
    }
    let fake = Fake::new();
    // G32CxxLeaf : G32CxxBase : G32HostCxxRoot. Both game classes have C++
    // ivars (.cxx_construct / .cxx_destruct); the leaf has -init and a
    // -dealloc that calls [super dealloc].
    let base = class32(
        fake,
        "G32CxxBase",
        ClassTarget::External("G32HostCxxRoot".into()),
        "G32HostCxxRoot",
        vec![method(".cxx_construct", "@8@0:4", 0x2401), method(".cxx_destruct", "v8@0:4", 0x2402)],
    );
    let leaf = class32(
        fake,
        "G32CxxLeaf",
        ClassTarget::Local(base.address),
        "G32HostCxxRoot",
        vec![
            method(".cxx_construct", "@8@0:4", 0x2411),
            method(".cxx_destruct", "v8@0:4", 0x2412),
            method("init", "@8@0:4", 0x2413),
            method("dealloc", "v8@0:4", 0x2414),
        ],
    );
    let leaf_class = leaf.address;
    let md = ObjcMetadata32 { classes: vec![base, leaf], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let note = |event: &'static str| move |w: &[u32]| {
        CXX_EVENTS.lock().unwrap().push(event);
        u64::from(w[0])
    };
    fake.code(0x2401, note("construct base"));
    fake.code(0x2411, note("construct leaf"));
    fake.code(0x2402, note("destruct base"));
    fake.code(0x2412, note("destruct leaf"));
    fake.code(0x2413, note("init"));
    let super_struct = fake.alloc_permanent(8);
    fake.code(0x2414, move |w| {
        CXX_EVENTS.lock().unwrap().push("dealloc");
        fake.write(super_struct, &[w[0].to_le_bytes(), leaf_class.to_le_bytes()].concat());
        let words = [super_struct, w[1]];
        send_from_guest(bridge, fake, SendKind::Super2, &|i| words[i as usize]).unwrap();
        0
    });
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let events = || std::mem::take(&mut *CXX_EVENTS.lock().unwrap());

    // +alloc: the constructors run, superclass first, before the guest sees the object.
    let object = send(bridge, fake, &[leaf_class, sel("alloc")]).unwrap() as u32;
    assert_eq!(events(), ["construct base", "construct leaf"]);
    // +new (alloc then init in the host): constructed before -init runs.
    send(bridge, fake, &[leaf_class, sel("new")]).unwrap();
    assert_eq!(events(), ["construct base", "construct leaf", "init"]);
    // -dealloc: the game's body, then at [super dealloc] the destructors,
    // subclass first, then the framework's dealloc; the guest memory goes.
    send(bridge, fake, &[object, sel("dealloc")]).unwrap();
    assert_eq!(events(), ["dealloc", "destruct leaf", "destruct base", "host dealloc"]);
    assert!(fake.freed.lock().unwrap().contains(&object));
}

/// UIProgressView's progress is a C float: a 32-bit game's setProgress:
/// (and setProgress:animated:) reach it with the value it sent.
#[test]
fn progress_bars_get_their_float_progress_from_32_bit_games() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let bar = runtime::send(runtime::send(runtime::class_named("UIProgressView"), "alloc", &[]), "init", &[]);
    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap());
    let proxy = bridge.lock().unwrap().guest_object(fake, bar).unwrap();
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    send(&bridge, fake, &[proxy, sel("setProgress:"), 0.5f32.to_bits()]).unwrap();
    assert_eq!(crate::fw::progress::progress(bar), 0.5);
    send(&bridge, fake, &[proxy, sel("setProgress:animated:"), 0.25f32.to_bits(), 1]).unwrap();
    assert_eq!(crate::fw::progress::progress(bar), 0.25);
    let back = send(&bridge, fake, &[proxy, sel("progress")]).unwrap() as u32;
    assert_eq!(f32::from_bits(back), 0.25, "read back as a float");
}

/// Data that only looks like a pointer (Mach's NDR_record reads as
/// 0x1_0000_0000; nothing is mapped there) is not an object, and checking
/// it must not read that memory.
#[test]
fn values_pointing_at_unmapped_memory_are_not_host_objects() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    assert!(!is_host_object(0x1_0000_0000));
    let page = unsafe {
        libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_NONE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
    };
    assert_ne!(page, libc::MAP_FAILED);
    assert!(!is_host_object(page as usize), "a page that can't be read");
    let object = runtime::send(runtime::class_named("NSObject"), "new", &[]);
    assert!(is_host_object(object), "a real object still counts");
}

/// -sortUsingFunction:context: / -sortedArrayUsingFunction:context: from a
/// 32-bit game: the comparator is game code, called with the elements' guest
/// ids and the game's context.
#[test]
fn framework_arrays_sort_with_a_game_function() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap())));
    let numbers = |values: &[usize]| {
        let list = runtime::send(runtime::class_named("NSMutableArray"), "new", &[]);
        for &n in values {
            runtime::send(list, "addObject:", &[runtime::send(runtime::class_named("NSNumber"), "numberWithInt:", &[n])]);
        }
        list
    };
    let values = |list: Id| -> Vec<i64> {
        let count = runtime::send(list, "count", &[]);
        (0..count).map(|i| crate::fw::collections::number_as_f64(runtime::send(list, "objectAtIndex:", &[i])) as i64).collect()
    };
    let contexts: &'static Mutex<Vec<u32>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    // Largest first: NSOrderedAscending (-1) when the first is larger.
    fake.code(0x4201, move |w| {
        contexts.lock().unwrap().push(w[2]);
        let value = |guest: u32| crate::fw::collections::number_as_f64(bridge.lock().unwrap().host_object_for(fake, guest).unwrap());
        let (a, b) = (value(w[0]), value(w[1]));
        (if a > b { -1i32 } else if a < b { 1 } else { 0 }) as u32 as u64
    });
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let list = numbers(&[3, 1, 4, 1, 5]);
    let proxy = bridge.lock().unwrap().guest_object(fake, list).unwrap();
    send(bridge, fake, &[proxy, sel("sortUsingFunction:context:"), 0x4201, 77]).unwrap();
    assert_eq!(values(list), [5, 4, 3, 1, 1]);
    let other = numbers(&[2, 9, 7]);
    let proxy = bridge.lock().unwrap().guest_object(fake, other).unwrap();
    let sorted = send(bridge, fake, &[proxy, sel("sortedArrayUsingFunction:context:"), 0x4201, 77]).unwrap() as u32;
    assert_eq!(values(bridge.lock().unwrap().host_object_for(fake, sorted).unwrap()), [9, 7, 2]);
    assert_eq!(values(other), [2, 9, 7], "the receiver stays as it was");
    let contexts = contexts.lock().unwrap();
    assert!(!contexts.is_empty() && contexts.iter().all(|&c| c == 77), "{contexts:?}");
}

#[test]
fn a_tutorial_waiting_for_a_skipped_story_box_goes_on() {
    // SpellCraft: a story box can line up a next box that shows only once.
    // After a quit mid-tutorial that next box was seen already, so the game
    // skips it, and the tutorial (which waits for that box to close) kept
    // the map locked. Nothing showing or lined up after a box with a next
    // box closes: the tutorial is told to go on.
    use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    host_class("G32HostStoryBase");
    let fake = Fake::new();
    let base = || ClassTarget::External("G32HostStoryBase".into());
    let class = |name: &str, methods| class32(fake, name, base(), "G32HostStoryBase", methods);
    let alert = class("SCAlertController", vec![method("dialogViewDidClose:", "v12@0:4@8", 0x2301), method("currentAlert", "@8@0:4", 0x2311), method("alertQueue", "@8@0:4", 0x2321)]);
    let mut tutorial = class("SCTutorialController", vec![method("tutorialUnderway", "c8@0:4", 0x2341), method("beginStageInteraction", "v8@0:4", 0x2351)]);
    tutorial.meta.methods = vec![method("tutorialController", "@8@0:4", 0x2331)];
    let dialog = class("G32StoryDialog", vec![method("spec", "@8@0:4", 0x2361)]);
    let spec = class("G32StorySpec", vec![method("stringValueForKey:", "@12@0:4@8", 0x2371)]);
    let text = class("G32StoryText", vec![method("length", "I8@0:4", 0x2381)]);
    let queue = class("G32StoryQueue", vec![method("count", "I8@0:4", 0x2391)]);
    let classes = [&alert, &tutorial, &dialog, &spec, &text, &queue].map(|c| c.address);
    let md = ObjcMetadata32 { classes: vec![alert, tutorial, dialog, spec, text, queue], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let [alert, tutorial, dialog, spec, text, queue] = classes.map(|c| send(bridge, fake, &[c, sel("alloc")]).unwrap() as u32);
    static SHOWING: AtomicU32 = AtomicU32::new(0);
    static NEXT_NAME: AtomicU32 = AtomicU32::new(0);
    static BEGUN: AtomicU32 = AtomicU32::new(0);
    fake.code(0x2301, |_| 0);
    fake.code(0x2311, |_| u64::from(SHOWING.load(SeqCst)));
    fake.code(0x2321, move |_| u64::from(queue));
    fake.code(0x2391, |_| 0);
    fake.code(0x2331, move |_| u64::from(tutorial));
    fake.code(0x2341, |_| 1);
    fake.code(0x2351, |_| u64::from(BEGUN.fetch_add(1, SeqCst)));
    fake.code(0x2361, move |_| u64::from(spec));
    fake.code(0x2371, move |_| u64::from(text));
    fake.code(0x2381, |_| u64::from(NEXT_NAME.load(SeqCst)));
    let close = || send(bridge, fake, &[alert, sel("dialogViewDidClose:"), dialog]).unwrap();
    // No next box: the tutorial's own -dialogViewDidClose: goes on.
    close();
    assert_eq!(BEGUN.load(SeqCst), 0);
    // A next box that is showing: the tutorial waits for it.
    NEXT_NAME.store(9, SeqCst);
    SHOWING.store(dialog, SeqCst);
    close();
    assert_eq!(BEGUN.load(SeqCst), 0);
    // A next box that was skipped: told to go on (each time, not only the first).
    SHOWING.store(0, SeqCst);
    close();
    close();
    assert_eq!(BEGUN.load(SeqCst), 2);
}
