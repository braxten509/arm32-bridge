//! The Objective-C runtime C functions and blocks, on the made-up guest.

use super::bridge::{install, set_block_owner, Bridge, GuestSide, Special};
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
    // object_setIvar stores without retaining; object_getIvar reads it back.
    call("object_setIvar", &[object, ivar, proxy]);
    assert_eq!((g.u32_at(object + 8), call("object_getIvar", &[object, ivar])), (proxy, proxy));

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
    // The host wrapper holds its own reference: the guest's last release
    // (a completion handler handed over, then released) keeps the block alive.
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 4);
    call("_Block_release", &[heap]);
    assert!(!g.freed.lock().unwrap().contains(&heap), "the host still holds the block");
    assert_eq!(mirror::call_host(invoke, &args).x[0] as u32, 111);
    // Messages to a guest block: copy keeps it, release lets it go.
    let copy_sel = bridge.lock().unwrap().guest_selector(g, "copy");
    assert_eq!(call("objc_msgSend", &[heap, copy_sel]), heap);
    assert_eq!(g.u32_at(heap + 4) & 0xffff, 4);
}

#[test]
fn block_counts_change_atomically_and_saturated_counts_stay() {
    use super::bridge::BLOCK_NEEDS_FREE;
    let g = Fake::new();
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap())));
    let malloc_isa = g.words(&[0; 5]);
    bridge.lock().unwrap().set_special(malloc_isa, Special::MallocBlock);
    let call = move |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&Ctx { bridge, g }, name, &w).unwrap() as u32
    };
    let descriptor = g.words(&[0, 20]);
    let count = |at: u32| g.u32_at(at) & 0xfffe;
    // A heap block held once. Copies and releases from many threads at once
    // (the host's block helpers run without the guest lock) leave it as it was.
    let heap = g.words(&[malloc_isa, BLOCK_NEEDS_FREE | 2, 0, 0x4301, descriptor]);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    call("_Block_copy", &[heap]);
                    call("_Block_release", &[heap]);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(count(heap + 4), 2, "no reference was lost or added");
    assert!(!g.freed.lock().unwrap().contains(&heap), "a live block was never freed");
    // A saturated count stays: neither a release nor a copy changes it, and the block is never freed.
    let pinned = g.words(&[malloc_isa, BLOCK_NEEDS_FREE | 0xfffe, 0, 0x4301, descriptor]);
    call("_Block_release", &[pinned]);
    call("_Block_copy", &[pinned]);
    assert_eq!(count(pinned + 4), 0xfffe);
    assert!(!g.freed.lock().unwrap().contains(&pinned));
    // The same for a __block variable on the heap: { isa, forwarding, flags, size }.
    let byref = g.words(&[0, 0, BLOCK_NEEDS_FREE | 0xfffe, 16]);
    g.put_words(byref + 4, &[byref]);
    call("_Block_object_dispose", &[byref, 8]);
    let slot = g.words(&[0]);
    call("_Block_object_assign", &[slot, byref, 8]);
    assert_eq!((count(byref + 8), g.u32_at(slot)), (0xfffe, byref));
    assert!(!g.freed.lock().unwrap().contains(&byref));
}

#[test]
fn autorelease_pools_belong_to_their_thread() {
    // Thread A pushes, thread B pushes, A pops, then B pops: B's pool is still its own.
    fn pool(name: &str, token: u32) -> Result<u64, String> {
        let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap())));
        let words = [token];
        call_function(&Ctx { bridge, g: Fake::new() }, name, &|i| words.get(i as usize).copied().unwrap_or(0))
    }
    let a = pool("objc_autoreleasePoolPush", 0).unwrap() as u32;
    let (pushed, popped) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let b = std::thread::spawn(move || {
        let token = pool("objc_autoreleasePoolPush", 0).unwrap() as u32;
        pushed.0.send(()).unwrap();
        popped.1.recv().unwrap();
        pool("objc_autoreleasePoolPop", token)
    });
    pushed.1.recv().unwrap();
    pool("objc_autoreleasePoolPop", a).unwrap();
    popped.0.send(()).unwrap();
    assert!(b.join().unwrap().is_ok(), "thread B's pool survived thread A's pop");
}

static KEPT: Mutex<usize> = Mutex::new(0);

extern "C" fn host_sort(this: Id, _: Sel, block: usize) -> Id {
    // Runs the comparator now and keeps nothing.
    let invoke = unsafe { *((block + 16) as *const usize) };
    let mut args = HostArgs::default();
    args.x[..3].copy_from_slice(&[block as u64, 5, 6]);
    mirror::call_host(invoke, &args);
    this
}

extern "C" fn host_keep(_: Id, _: Sel, block: usize) {
    *KEPT.lock().unwrap() = crate::libsystem::mach::block_copy(block as *mut _) as usize;
}

#[test]
fn exported_blocks_live_as_long_as_their_host_owners() {
    let host_class = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32HostKeeper", 0, 16);
        let (sort, keep) = (rt.sel("sortedArrayUsingComparator:"), rt.sel("setCompletionHandler:"));
        rt.add_method(class, sort, host_sort as usize);
        rt.add_method(class, keep, host_keep as usize);
        class
    };
    let g = Fake::new();
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&ObjcMetadata32::default()).unwrap())));
    let (stack_isa, malloc_isa) = (g.words(&[0; 5]), g.words(&[0; 5]));
    {
        let mut b = bridge.lock().unwrap();
        b.set_special(stack_isa, Special::StackBlock);
        b.set_special(malloc_isa, Special::MallocBlock);
    }
    let ctx = Ctx { bridge, g };
    let call = |name: &str, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        call_function(&ctx, name, &w).unwrap() as u32
    };
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame| super::bridge::dispatch_from_host(bridge, g, frame).unwrap()));
    set_block_owner(g);
    let runs: &'static Mutex<u32> = Box::leak(Box::new(Mutex::new(0)));
    g.code(0x4101, move |w| {
        *runs.lock().unwrap() += 1;
        u64::from(w[1] + w[2])
    });
    let descriptor = g.words(&[0, 20, g.text("i12@?0i4i8")]);
    let heap = call("_Block_copy", &[g.words(&[stack_isa, 1 << 30, 0, 0x4101, descriptor])]);
    let count = || g.u32_at(heap + 4) & 0xffff;
    assert_eq!(count(), 2);
    let keeper = bridge.lock().unwrap().guest_object(g, runtime::alloc_instance(host_class)).unwrap();
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(g, name);
    // Used during the call only: nothing is held afterwards.
    call("objc_msgSend", &[keeper, sel("sortedArrayUsingComparator:"), heap]);
    assert_eq!((*runs.lock().unwrap(), count()), (1, 2), "the call's wrapper let go of the block");
    // Kept by the host (it copied the block): that copy holds a reference.
    call("objc_msgSend", &[keeper, sel("setCompletionHandler:"), heap]);
    assert_eq!(count(), 4);
    call("_Block_release", &[heap]);
    assert!(!g.freed.lock().unwrap().contains(&heap));
    let kept = *KEPT.lock().unwrap();
    let invoke = unsafe { *((kept + 16) as *const usize) };
    let mut args = HostArgs::default();
    args.x[..3].copy_from_slice(&[kept as u64, 1, 2]);
    assert_eq!(mirror::call_host(invoke, &args).x[0] as u32, 3);
    // When the host's copy is disposed, the guest block goes too.
    let dispose: extern "C" fn(usize) = unsafe { std::mem::transmute(*((*((kept + 24) as *const usize) + 24) as *const usize)) };
    dispose(kept);
    assert!(g.freed.lock().unwrap().contains(&heap));
}

extern "C" fn nothing(_: Id, _: Sel) {}

#[test]
fn a_game_objects_dealloc_releases_its_associated_objects_and_clears_weak_references() {
    // Host root with counted retain/release and a dealloc; game class G32Dying on it.
    let host_class = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32DyingRoot", 0, 16);
        let meta = rt.classes[&class].partner;
        for (target, name, imp) in [
            (meta, "alloc", alloc as usize),
            (class, "retain", retain as usize),
            (class, "release", release as usize),
            (class, "dealloc", nothing as usize),
        ] {
            let sel = rt.sel(name);
            rt.add_method(target, sel, imp);
        }
        class
    };
    let g = Fake::new();
    let (class, meta) = (g.alloc_permanent(20), g.alloc_permanent(20));
    g.put_words(class, &[meta]);
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Dying".into(),
            superclass: ClassTarget::External("G32DyingRoot".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("G32DyingRoot".into()),
            meta_superclass: ClassTarget::External("G32DyingRoot".into()),
            instance: ClassRo32 { instance_start: 8, instance_size: 16, ..Default::default() },
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
    let object = call("objc_msgSend", &[class, sel("alloc")]);
    let survivor = call("objc_msgSend", &[class, sel("alloc")]);
    // A retained associated value (a host object counting its releases).
    let value_host = runtime::alloc_instance(host_class);
    let value = bridge.lock().unwrap().guest_object(g, value_host).unwrap();
    call("objc_setAssociatedObject", &[object, 0x55, value, 1]);
    assert_eq!(events(value_host), ["retain"]);
    // Weak references: initialized, moved, and one to another object.
    let (weak, moved_from, moved_to, other) = (g.words(&[0]), g.words(&[0]), g.words(&[0]), g.words(&[0]));
    call("objc_initWeak", &[weak, object]);
    call("objc_initWeak", &[moved_from, object]);
    call("objc_moveWeak", &[moved_to, moved_from]);
    call("objc_storeWeak", &[other, survivor]);

    call("objc_msgSend", &[object, sel("dealloc")]);
    assert_eq!(events(value_host), ["retain", "release"], "the associated value was released once");
    assert_eq!(call("objc_getAssociatedObject", &[object, 0x55]), 0);
    assert_eq!(call("objc_loadWeak", &[weak]), 0, "weak references to it read nil");
    assert_eq!(call("objc_loadWeak", &[moved_to]), 0, "including a moved one");
    assert_eq!(call("objc_loadWeak", &[other]), survivor, "a weak reference to another object stays");
}

/// +instanceMethodForSelector: and -methodForSelector: of a game class give
/// the game's own function, which the game then calls itself (cocos2d's
/// scheduler does); a framework method gives objc_msgSend's stub, which
/// reaches it the same way.
#[test]
fn method_for_selector_gives_the_games_own_function() {
    let host_class = {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32PlainRoot", 0, 16);
        let meta = rt.classes[&class].partner;
        let sel = rt.sel("alloc");
        rt.add_method(meta, sel, alloc as usize);
        let sel = rt.sel("bounds");
        rt.add_method(class, sel, plain_root_bounds as usize);
        class
    };
    let _ = host_class;
    let g = Fake::new();
    let (class, meta) = (g.alloc_permanent(20), g.alloc_permanent(20));
    g.put_words(class, &[meta]);
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Timer".into(),
            superclass: ClassTarget::External("G32PlainRoot".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("G32PlainRoot".into()),
            meta_superclass: ClassTarget::External("G32PlainRoot".into()),
            instance: ClassRo32 { instance_start: 8, instance_size: 8, ..Default::default() },
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
    g.code(0x3101, |w| u64::from(w[2]) * 2);
    assert_eq!(call("class_addMethod", &[class, sel("update:"), 0x3101, g.text("v12@0:4f8")]), 1);
    let object = call("objc_msgSend", &[class, sel("alloc")]);
    assert_eq!(call("objc_msgSend", &[class, sel("instanceMethodForSelector:"), sel("update:")]), 0x3101);
    assert_eq!(call("objc_msgSend", &[object, sel("methodForSelector:"), sel("update:")]), 0x3101);
    // A framework method: objc_msgSend's own stub, or for a struct result
    // (CGRect, returned through a hidden first pointer) objc_msgSend_stret's.
    let stub = call("objc_msgSend", &[class, sel("instanceMethodForSelector:"), sel("description")]);
    assert_eq!((stub, g.function_at(stub).as_deref()), (g.function_address("objc_msgSend"), Some("objc_msgSend")));
    let stret = call("objc_msgSend", &[object, sel("methodForSelector:"), sel("bounds")]);
    assert_eq!(g.function_at(stret).as_deref(), Some("objc_msgSend_stret"), "a CGRect result");
    // Called as the game calls it: result pointer, receiver, selector.
    let result = g.alloc_permanent(16);
    let w = |words: Vec<u32>| move |i: u32| words.get(i as usize).copied().unwrap_or(0);
    call_function(&ctx, &g.function_at(stret).unwrap(), &w(vec![result, object, sel("bounds")])).unwrap();
    let floats: Vec<f32> = (0..4).map(|i| f32::from_bits(g.u32_at(result + 4 * i))).collect();
    assert_eq!(floats, [1.0, 2.0, 3.0, 4.0]);
}

/// NSAllocateObject makes a zeroed game instance with room for its extra
/// bytes; NSGetSizeAndAlignment measures armv7 layouts; CFStringAppendFormat
/// formats the game's variadic arguments, objects included.
#[test]
fn foundation_allocation_layout_and_format_functions() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    {
        let mut rt = runtime::lock();
        let class = rt.define_class("G32AllocRoot", 0, 16);
        let meta = rt.classes[&class].partner;
        let sel = rt.sel("alloc");
        rt.add_method(meta, sel, alloc as usize);
    }
    let g = Fake::new();
    let (class, meta) = (g.alloc_permanent(20), g.alloc_permanent(20));
    g.put_words(class, &[meta]);
    let md = ObjcMetadata32 {
        classes: vec![ObjcClass32 {
            address: class,
            name: "G32Allocated".into(),
            superclass: ClassTarget::External("G32AllocRoot".into()),
            meta_address: meta,
            meta_isa: ClassTarget::External("G32AllocRoot".into()),
            meta_superclass: ClassTarget::External("G32AllocRoot".into()),
            instance: ClassRo32 { instance_start: 4, instance_size: 12, ..Default::default() },
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

    let zone = call("NSDefaultMallocZone", &[]);
    assert_ne!(zone, 0);
    let object = call("NSAllocateObject", &[class, 40, zone]);
    assert_eq!(g.u32_at(object), class, "an instance of the game class");
    assert_eq!(g.read(object + 4, 48), [0; 48], "its own and extra bytes, zeroed");
    let host = bridge.lock().unwrap().host_object(object).expect("a host side");
    assert_eq!(bridge.lock().unwrap().guest_object(g, host).unwrap(), object);
    let next = g.alloc(4);
    assert!(next >= object + 52, "the extra bytes are the object's ({object:#x}, next {next:#x})");

    // NSGetSizeAndAlignment(type, &size, &align) -> the type after it (offsets skipped).
    let (size, align) = (g.words(&[0]), g.words(&[0]));
    let types = g.text("{Pair=cd}12@0");
    let rest = call("NSGetSizeAndAlignment", &[types, size, align]);
    assert_eq!((g.u32_at(size), g.u32_at(align), rest), (12, 4, types + 11), "a double is 4-byte aligned on armv7");
    assert_eq!(call("NSGetSizeAndAlignment", &[rest, 0, align]), rest + 2, "NULL outputs are skipped");
    assert_eq!(g.u32_at(align), 4);

    // CFStringAppendFormat(string, options, format, ...)
    let mutable = runtime::send(runtime::class_named("NSMutableString"), "stringWithString:", &[crate::fw::string::make("x")]);
    let guest = |host: usize| bridge.lock().unwrap().guest_object(g, host).unwrap();
    let (target, format, word) = (guest(mutable), guest(crate::fw::string::make("%d-%@")), guest(crate::fw::string::make("hi")));
    call("CFStringAppendFormat", &[target, 0, format, 42, word]);
    assert_eq!(crate::fw::string::get(mutable), "x42-hi");
}

extern "C" fn plain_root_bounds(_: Id, _: Sel) -> crate::fw::geometry::Rect {
    crate::fw::geometry::Rect::new(1.0, 2.0, 3.0, 4.0)
}
