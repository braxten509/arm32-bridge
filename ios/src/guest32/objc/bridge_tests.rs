//! The bridge with a made-up guest (memory buffer + Rust closures standing in
//! for guest code) and real host classes in the 64-bit runtime.

use super::bridge::*;
use super::convert::{HostFrame, HostRet};
use super::mirror;
use super::testing::Fake;
use crate::objc::runtime::{self, Class, Id, Sel};
use guest32_image::{ClassRo32, ClassTarget, Method32, ObjcClass32, ObjcMetadata32};
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
