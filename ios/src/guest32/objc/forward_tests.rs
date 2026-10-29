//! Message forwarding for 32-bit games: game proxies (NSProxy subclasses)
//! in made-up metadata, real host Foundation classes behind them.

use super::bridge::*;
use super::convert::HostFrame;
use super::mirror;
use super::testing::Fake;
use crate::objc::runtime;
use guest32_image::{ClassRo32, ClassTarget, Method32, ObjcClass32, ObjcMetadata32};
use std::sync::Mutex;

fn method(name: &str, types: &str, imp: u32) -> Method32 {
    Method32 { name: name.into(), types: types.into(), imp }
}

fn class32(fake: &Fake, name: &str, root: &str, methods: Vec<Method32>) -> ObjcClass32 {
    let (class, meta) = (fake.alloc_permanent(20), fake.alloc_permanent(20));
    fake.write(class, &meta.to_le_bytes());
    ObjcClass32 {
        address: class,
        name: name.into(),
        superclass: ClassTarget::External(root.into()),
        meta_address: meta,
        meta_isa: ClassTarget::External(root.into()),
        meta_superclass: ClassTarget::External(root.into()),
        instance: ClassRo32 { instance_start: 4, instance_size: 8, methods, ..Default::default() },
        meta: ClassRo32 { flags: 1, ..Default::default() },
    }
}

const RECT: &str = "{CGRect={CGPoint=ff}{CGSize=ff}}";

/// G32FwdTarget : NSProxy hands every message to a dictionary
/// (-forwardingTargetForSelector:); G32FwdInvoker : NSProxy forwards
/// invocations to a G32FwdReal (-methodSignatureForSelector: and
/// -forwardInvocation: with -invokeWithTarget:).
#[test]
fn proxies_forward_by_target_and_by_invocation_with_armv7_arguments() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let target = class32(fake, "G32FwdTarget", "NSProxy", vec![method("forwardingTargetForSelector:", "@12@0:4:8", 0x3001)]);
    let invoker = class32(
        fake,
        "G32FwdInvoker",
        "NSProxy",
        vec![method("methodSignatureForSelector:", "@12@0:4:8", 0x3011), method("forwardInvocation:", "v12@0:4@8", 0x3021)],
    );
    let real = class32(
        fake,
        "G32FwdReal",
        "NSObject",
        vec![
            method("label", "@8@0:4", 0x3031),
            method("twice:", "d16@0:4d8", 0x3041),
            method("rectScaled:", &format!("{RECT}12@0:4f8"), 0x3051),
        ],
    );
    let (target_class, invoker_class, real_class) = (target.address, invoker.address, real.address);
    let md = ObjcMetadata32 { classes: vec![target, invoker, real], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    super::forward::set_invocation_hooks(bridge, fake);
    let ctx = super::functions::Ctx { bridge, g: fake };
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let guest = |host: usize| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let host = |guest: u32| bridge.lock().unwrap().host_object_for(fake, guest).unwrap();
    let send = |kind: SendKind, words: &[u32]| {
        let w = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        send_from_guest(bridge, fake, kind, &w)
    };

    // -forwardingTargetForSelector: answers with a host dictionary, which
    // then gets -objectForKey: with the same argument.
    let dictionary = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
    runtime::send(dictionary, "setObject:forKey:", &[crate::fw::string::make("value"), crate::fw::string::make("key")]);
    let dictionary_guest = guest(dictionary);
    fake.code(0x3001, move |_| u64::from(dictionary_guest));
    let proxy = ctx.send(target_class, "alloc", &[]).unwrap();
    assert_eq!(fake.u32_at(proxy), target_class);
    assert_eq!(ctx.send(proxy, "isProxy", &[]).unwrap() & 0xff, 1, "an NSProxy says it is one");
    let key = guest(crate::fw::string::make("key"));
    let value = send(SendKind::Normal, &[proxy, sel("objectForKey:"), key]).unwrap().r0;
    assert_eq!(crate::fw::string::get(host(value)), "value");

    // Invocations: the signature comes from the real object (armv7 types of
    // its game methods), the invocation is sent on to it.
    let real_object = ctx.send(real_class, "alloc", &[]).unwrap();
    let invoker_object = ctx.send(invoker_class, "alloc", &[]).unwrap();
    let unknown = sel("frobnicate:");
    fake.code(0x3011, move |w| {
        if w[2] == unknown {
            return 0;
        }
        u64::from(super::functions::Ctx { bridge, g: fake }.send(real_object, "methodSignatureForSelector:", &[w[2]]).unwrap())
    });
    fake.code(0x3021, move |w| {
        super::functions::Ctx { bridge, g: fake }.send(w[2], "invokeWithTarget:", &[real_object]).unwrap();
        0
    });
    let label = guest(crate::fw::string::make("real label"));
    fake.code(0x3031, move |_| u64::from(label));
    fake.code(0x3041, |w| {
        let x = f64::from_bits(u64::from(w[2]) | u64::from(w[3]) << 32);
        (x * 2.0).to_bits()
    });
    fake.code(0x3051, move |w| {
        let scale = f32::from_bits(w[3]);
        let rect: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0].iter().flat_map(|v| (v * scale).to_le_bytes()).collect();
        fake.write(w[0], &rect);
        u64::from(w[0])
    });

    // (a) an object result
    assert_eq!(send(SendKind::Normal, &[invoker_object, sel("label")]).unwrap().r0, label);
    // (b) a double argument (words 2-3) and a double result (r0:r1)
    let x = 1.25f64.to_bits();
    let r = send(SendKind::Normal, &[invoker_object, sel("twice:"), x as u32, (x >> 32) as u32]).unwrap();
    assert_eq!(f64::from_bits(u64::from(r.r0) | u64::from(r.r1) << 32), 2.5);
    // (c) a CGRect result through the hidden pointer in r0
    let out = fake.alloc_permanent(16);
    let r = send(SendKind::Stret, &[out, invoker_object, sel("rectScaled:"), 2.0f32.to_bits()]).unwrap();
    assert_eq!(r.r0, out);
    let floats: Vec<f32> = fake.read(out, 16).chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(floats, [2.0, 4.0, 6.0, 8.0]);
    // NSProxy's -respondsToSelector: is forwarded like any other message.
    assert_eq!(send(SendKind::Normal, &[invoker_object, sel("respondsToSelector:"), sel("twice:")]).unwrap().r0 & 0xff, 1);

    // No signature: -doesNotRecognizeSelector:, Apple's exception thrown in the guest.
    assert!(take_pending_throw().is_none());
    send(SendKind::Normal, &[invoker_object, unknown, 7]).unwrap();
    let exception = take_pending_throw().expect("an exception for the guest to throw");
    let name = runtime::send(host(exception), "name", &[]);
    assert_eq!(crate::fw::string::get(name), "NSInvalidArgumentException");
}

/// A proxy's message that only forwarding answers (SpellCraft's
/// AEAssetProxy, ~4000 a session) is remembered: later ones ask
/// -forwardingTargetForSelector: straight away, once each, and its answer
/// still decides every time; a nil answer goes on to Apple's later steps;
/// a new method anywhere ends it.
#[test]
fn a_forwarded_message_skips_the_lookups_next_time_but_still_asks_each_time() {
    use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(crate::run::setup);
    let fake = Fake::new();
    let memo = class32(fake, "G32FwdMemo", "NSProxy", vec![method("forwardingTargetForSelector:", "@12@0:4:8", 0x3101)]);
    let memo_class = memo.address;
    let md = ObjcMetadata32 { classes: vec![memo], ..Default::default() };
    let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
    install(bridge, fake, &md).unwrap();
    let _hook = mirror::DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    mirror::set_dispatch(Box::new(move |frame: &mut HostFrame| {
        dispatch_from_host(bridge, fake, frame).unwrap();
    }));
    super::forward::set_invocation_hooks(bridge, fake);
    let ctx = super::functions::Ctx { bridge, g: fake };
    let sel = |name: &str| bridge.lock().unwrap().guest_selector(fake, name);
    let guest = |host: usize| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let host = |guest: u32| bridge.lock().unwrap().host_object_for(fake, guest).unwrap();
    let dictionary = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
    runtime::send(dictionary, "setObject:forKey:", &[crate::fw::string::make("value"), crate::fw::string::make("key")]);
    static ASKED: AtomicU32 = AtomicU32::new(0);
    static ANSWER: AtomicU32 = AtomicU32::new(0);
    ANSWER.store(guest(dictionary), SeqCst);
    fake.code(0x3101, |_| {
        ASKED.fetch_add(1, SeqCst);
        u64::from(ANSWER.load(SeqCst))
    });
    let proxy = ctx.send(memo_class, "alloc", &[]).unwrap();
    let (object_for_key, key) = (sel("objectForKey:"), guest(crate::fw::string::make("key")));
    let lookup = || {
        let words = [proxy, object_for_key, key];
        send_from_guest(bridge, fake, SendKind::Normal, &|i| words.get(i as usize).copied().unwrap_or(0)).unwrap().r0
    };
    for round in 0..2 {
        assert_eq!(crate::fw::string::get(host(lookup())), "value", "round {round}");
    }
    assert_eq!(ASKED.load(SeqCst), 2, "asked once a message");
    assert!(bridge.lock().unwrap().classes.cached_forward(memo_class, object_for_key).is_some(), "remembered");
    // A nil answer: on to -methodSignatureForSelector: (NSProxy's raises).
    ANSWER.store(0, SeqCst);
    assert!(take_pending_throw().is_none());
    lookup();
    assert!(take_pending_throw().is_some(), "Apple's later steps ran");
    assert_eq!(ASKED.load(SeqCst), 3, "not asked twice");
    assert!(bridge.lock().unwrap().classes.add_method(memo_class, "glow", 0x3111, "v8@0:4"));
    assert!(bridge.lock().unwrap().classes.cached_forward(memo_class, object_for_key).is_none(), "a new method ends it");
}
