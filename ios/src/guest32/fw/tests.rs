//! Behavior that could silently break at the 32-bit framework boundary:
//! structs passed by value across r0-r3 and the stack, struct results
//! through the hidden pointer, host objects and handles seen as guest
//! words, and 32-bit struct layouts that hold pointers.

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
