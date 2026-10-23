//! Tests for the guest class table on made-up metadata.

use super::classes::*;
use guest32_image::{ClassRo32, ClassTarget, Method32, ObjcCategory32, ObjcClass32, ObjcMetadata32};

fn methods(list: &[(&str, u32)]) -> Vec<Method32> {
    list.iter().map(|(n, imp)| Method32 { name: n.to_string(), types: "v8@0:4".into(), imp: *imp }).collect()
}

fn class(address: u32, name: &str, superclass: ClassTarget, instance: &[(&str, u32)], class_methods: &[(&str, u32)]) -> ObjcClass32 {
    let meta_superclass = match &superclass {
        ClassTarget::Local(a) => ClassTarget::Local(a + 0x100),
        other => other.clone(),
    };
    ObjcClass32 {
        address,
        name: name.into(),
        superclass,
        meta_address: address + 0x100,
        meta_isa: ClassTarget::External("NSObject".into()),
        meta_superclass,
        instance: ClassRo32 { name: name.into(), instance_size: 64, methods: methods(instance), ..Default::default() },
        meta: ClassRo32 { name: name.into(), flags: 1, methods: methods(class_methods), ..Default::default() },
    }
}

fn metadata() -> ObjcMetadata32 {
    ObjcMetadata32 {
        classes: vec![
            class(0x1000, "GameView", ClassTarget::External("UIView".into()), &[("draw", 0x11), ("layout", 0x12)], &[("make", 0x13)]),
            class(0x2000, "BossView", ClassTarget::Local(0x1000), &[("draw", 0x21)], &[]),
        ],
        categories: vec![
            ObjcCategory32 {
                address: 0x3000,
                name: "Extras".into(),
                class: ClassTarget::Local(0x1000),
                instance_methods: methods(&[("layout", 0x31), ("shake", 0x32)]),
                class_methods: methods(&[]),
                protocols: vec![],
                instance_properties: vec![],
            },
            ObjcCategory32 {
                address: 0x3100,
                name: "Hash".into(),
                class: ClassTarget::External("NSString".into()),
                instance_methods: methods(&[("md5", 0x41)]),
                class_methods: methods(&[]),
                protocols: vec![],
                instance_properties: vec![],
            },
        ],
        ..Default::default()
    }
}

#[test]
fn lookup_walks_game_classes_then_reports_the_framework_class() {
    let table = GuestClasses::from_metadata(&metadata()).unwrap();
    let found = |class, sel| match table.lookup(class, sel) {
        Lookup::Guest(m) => Some(m.imp),
        _ => None,
    };
    assert_eq!(found(0x2000, "draw"), Some(0x21));
    // Inherited, and a category replaces the class's own method.
    assert_eq!(found(0x2000, "layout"), Some(0x31));
    assert_eq!(found(0x2000, "shake"), Some(0x32));
    // Class methods live on the metaclass.
    assert_eq!(found(0x2100, "make"), Some(0x13));
    assert_eq!(table.lookup(0x2000, "addSubview:"), Lookup::Framework { class: "UIView".into(), meta: false });
    assert_eq!(table.lookup(0x2100, "alloc"), Lookup::Framework { class: "UIView".into(), meta: true });
    assert_eq!(table.class_named("BossView").map(|c| c.address), Some(0x2000));
    assert_eq!(table.class(0x2100).map(|c| c.meta), Some(true));
    // Categories on framework classes are kept by class name.
    assert_eq!(table.framework_category_method("NSString", false, "md5").map(|m| m.imp), Some(0x41));
    // Every selector a game class answers, for its host mirror.
    let mut own = table.selectors_of(0x1000);
    own.sort();
    assert_eq!(own, ["draw", "layout", "shake"]);
}

#[test]
fn methods_added_at_run_time_are_found_and_not_doubled() {
    let mut table = GuestClasses::from_metadata(&metadata()).unwrap();
    assert!(table.add_method(0x2000, "jump", 0x51, "v8@0:4"));
    assert!(!table.add_method(0x2000, "jump", 0x52, "v8@0:4"));
    assert!(matches!(table.lookup(0x2000, "jump"), Lookup::Guest(m) if m.imp == 0x51));
    assert_eq!(table.lookup(0x1000, "jump"), Lookup::Framework { class: "UIView".into(), meta: false });
}
