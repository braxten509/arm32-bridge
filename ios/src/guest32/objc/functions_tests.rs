//! The Objective-C runtime C functions and blocks, on the made-up guest.

use super::bridge::{install, Bridge, GuestSide, Special};
use super::convert::HostArgs;
use super::functions::{call_function, Ctx};
use super::mirror;
use super::testing::Fake;
use crate::objc::runtime::{self, Class, Id, Sel};
use guest32_image::{ClassRo32, ClassTarget, Ivar32, ObjcClass32, ObjcMetadata32};
use std::sync::Mutex;

static COUNTS: Mutex<Vec<(Id, &'static str)>> = Mutex::new(Vec::new());

extern "C" fn alloc(class: Class, _: Sel) -> Id {
    runtime::alloc_instance(class)
}
extern "C" fn retain(this: Id, _: Sel) -> Id {
    COUNTS.lock().unwrap().push((this, "retain"));
    this
}
extern "C" fn release(this: Id, _: Sel) {
    COUNTS.lock().unwrap().push((this, "release"));
}
extern "C" fn copy(this: Id, _: Sel) -> Id {
    COUNTS.lock().unwrap().push((this, "copy"));
    let class = runtime::lock().class_of(this);
    runtime::alloc_instance(class)
}
extern "C" fn same(this: Id, _: Sel) -> Id {
    this
}

fn counted_class() -> Class {
    let mut rt = runtime::lock();
    let class = rt.define_class("G32Counted", 0, 16);
    let meta = rt.classes[&class].partner;
    for (target, name, imp) in [
        (meta, "alloc", alloc as usize),
        (class, "retain", retain as usize),
        (class, "release", release as usize),
        (class, "copy", copy as usize),
        (class, "autorelease", same as usize),
    ] {
        let sel = rt.sel(name);
        rt.add_method(target, sel, imp);
    }
    class
}

fn events(object: Id) -> Vec<&'static str> {
    COUNTS.lock().unwrap().iter().filter(|e| e.0 == object).map(|e| e.1).collect()
}

#[test]
fn runtime_functions_retain_store_properties_classes_and_ivars() {
    let host_class = counted_class();
    let g = Fake::new();
    let (class, meta) = (g.alloc_permanent(20), g.alloc_permanent(20));
    g.put_words(class, &[meta]);
    let offset_variable = g.words(&[8]);
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Holder".into(),
            superclass: ClassTarget::External("G32Counted".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("G32Counted".into()),
            meta_superclass: ClassTarget::External("G32Counted".into()),
            instance: ClassRo32 {
                instance_start: 8,
                instance_size: 16,
                ivars: vec![Ivar32 {
                    offset_address: offset_variable,
                    offset: 8,
                    name: "_thing".into(),
                    type_encoding: "@".into(),
                    alignment: 2,
                    size: 4,
                }],
                ..Default::default()
            },
            meta: ClassRo32 { flags: 1, ..Default::default() },
        }],
        ..Default::default()
    };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, g, &md).unwrap();
    let ctx = Ctx { bridge, g };
    let call = |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&ctx, name, &w).unwrap() as u32
    };
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(g, name);

    // retain/release reach the host object.
    let host = runtime::alloc_instance(host_class);
    let proxy = bridge.lock().unwrap().guest_object(g, host).unwrap();
    assert_eq!(call("objc_retain", &[proxy]), proxy);
    let slot = g.words(&[0]);
    call("objc_storeStrong", &[slot, proxy]);
    assert_eq!(g.u32_at(slot), proxy);
    call("objc_storeStrong", &[slot, 0]);
    assert_eq!(g.u32_at(slot), 0);
    assert_eq!(events(host), ["retain", "retain", "release"]);

    // Properties read and write the ivar at the given offset.
    let object = call("objc_msgSend", &[class, sel("alloc")]);
    g.put_words(object + 8, &[proxy]);
    assert_eq!(call("objc_getProperty", &[object, 0, 8, 0]), proxy);
    let other = bridge.lock().unwrap().guest_object(g, runtime::alloc_instance(host_class)).unwrap();
    call("objc_setProperty", &[object, 0, 8, other, 0, 1]);
    let copied = g.u32_at(object + 8);
    assert!(copied != other && copied != 0);
    assert!(events(bridge.lock().unwrap().host_object(other).unwrap()).contains(&"copy"));

    // Classes, superclasses, ivars.
    let name = g.text("G32Holder");
    assert_eq!(call("objc_getClass", &[name]), class);
    let framework = call("objc_getClass", &[g.text("G32Counted")]);
    assert_eq!(call("class_getSuperclass", &[class]), framework);
    assert_eq!(call("objc_getClass", &[g.text("G32NoSuchClass")]), 0);
    assert_eq!(call("object_getClass", &[object]), class);
    let ivar = call("class_getInstanceVariable", &[class, g.text("_thing")]);
    assert_eq!(call("ivar_getOffset", &[ivar]), 8);
    assert_eq!(g.read(call("ivar_getName", &[ivar]), 7), b"_thing\0");
    assert_eq!(call("class_getInstanceSize", &[class]), 16);

    // class_addMethod: the new method answers guest and host messages.
    g.code(0x3001, |w| u64::from(w[2]) + 1);
    assert_eq!(call("class_addMethod", &[class, sel("bump:"), 0x3001, g.text("i12@0:4i8")]), 1);
    assert_eq!(call("class_addMethod", &[class, sel("bump:"), 0x3001, g.text("i12@0:4i8")]), 0);
    assert_eq!(call("objc_msgSend", &[object, sel("bump:"), 41]), 42);
    let method = call("class_getInstanceMethod", &[class, sel("bump:")]);
    assert_eq!(g.u32_at(method + 8), 0x3001);

    // Associated objects are kept per (object, key).
    call("objc_setAssociatedObject", &[object, 0x77, proxy, 1]);
    assert_eq!(call("objc_getAssociatedObject", &[object, 0x77]), proxy);
    assert_eq!(call("objc_getAssociatedObject", &[object, 0x78]), 0);
}

#[test]
fn blocks_copy_to_the_heap_and_run_when_the_host_calls_them() {
    let g = Fake::new();
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap())));
    let (stack_isa, malloc_isa, global_isa) = (g.words(&[0; 5]), g.words(&[0; 5]), g.words(&[0; 5]));
    {
        let mut b = bridge.lock().unwrap();
        b.set_special(stack_isa, Special::StackBlock);
        b.set_special(malloc_isa, Special::MallocBlock);
        b.set_special(global_isa, Special::GlobalBlock);
    }
    let ctx = Ctx { bridge, g };
    let call = |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&ctx, name, &w).unwrap() as u32
    };
    // A stack block capturing one int (100), with a signature: int (^)(int, int).
    let signature = g.text("i12@?0i4i8");
    let descriptor = g.words(&[0, 24, signature]);
    let block = g.words(&[stack_isa, 1 << 30, 0, 0x4001, descriptor, 100]);
    let heap = call("_Block_copy", &[block]);
    assert_ne!(heap, block);
    assert_eq!(g.u32_at(heap), malloc_isa);
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 2);
    assert_eq!(g.u32_at(heap + 20), 100);
    assert_eq!(call("_Block_copy", &[heap]), heap);
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 4);
    call("_Block_release", &[heap]);
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 2);

    // The host gets a block whose invoke runs the guest's invoke.
    let gg: &'static Fake = g;
    g.code(0x4001, move |w| u64::from(w[1] + w[2] + gg.u32_at(w[0] + 20)));
    let host_block = bridge.lock().unwrap().host_object_for(g, heap).unwrap();
    let invoke = unsafe { *((host_block + 16) as *const usize) };
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame| super::bridge::dispatch_from_host(bridge, gg, frame).unwrap()));
    let mut args = HostArgs::default();
    args.x[..3].copy_from_slice(&[host_block as u64, 5, 6]);
    assert_eq!(mirror::call_host(invoke, &args).x[0] as u32, 111);
    // Messages to a guest block: copy keeps it, release lets it go.
    let copy_sel = bridge.lock().unwrap().guest_selector(g, "copy");
    assert_eq!(call("objc_msgSend", &[heap, copy_sel]), heap);
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 4);
}
