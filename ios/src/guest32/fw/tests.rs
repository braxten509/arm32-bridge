//! Behavior that could silently break at the 32-bit framework boundary:
//! structs passed by value across r0-r3 and the stack, struct results
//! through the hidden pointer, host objects and handles seen as guest
//! words, and 32-bit struct layouts that hold pointers.

use arm32_bridge::imports::gles;
use arm32_bridge::ios32;
use arm32_bridge::runtime::{self, mem, Args, GuestCpu};

fn setup() {
    crate::guest32::tests::setup();
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

// ----- Core Animation transforms -----

const IDENTITY_3D: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// Calls a CATransform3D function `name(&result, t, extra...)`: the result
/// pointer in r0, the 16 floats in r1-r3 and on the stack, then `extra`.
fn transform_3d(name: &str, t: [f32; 16], extra: &[f32]) -> Vec<f32> {
    let out = alloc(64);
    let words: Vec<u32> = t.iter().chain(extra).map(|v| v.to_bits()).collect();
    call(name, [out, words[0], words[1], words[2]], &words[3..]);
    floats(out, 16)
}

fn assert_matrix(got: &[f32], want: [f32; 16], what: &str) {
    assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5), "{what}: {got:?}, expected {want:?}");
}

#[test]
fn transform_3d_functions_give_apples_matrices() {
    // Rows m11..m14, ..., m41..m44; a point is a row vector on the left.
    let quarter = std::f32::consts::FRAC_PI_2;
    let turned = transform_3d("CATransform3DRotate", IDENTITY_3D, &[quarter, 0.0, 0.0, 1.0]);
    assert_matrix(&turned, [0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0], "90 degrees about z");
    // A zero axis turns nothing.
    assert_matrix(&transform_3d("CATransform3DRotate", IDENTITY_3D, &[quarter, 0.0, 0.0, 0.0]), IDENTITY_3D, "no axis");
    // The new step comes first (in the layer's own space): translating a
    // turned layer moves it along its turned x axis.
    let mut turned_16 = [0.0f32; 16];
    turned_16.copy_from_slice(&turned);
    let moved = transform_3d("CATransform3DTranslate", turned_16, &[5.0, 0.0, 0.0]);
    let mut want = [0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 5.0, 0.0, 1.0];
    assert_matrix(&moved, want, "translate after rotate");
    // ...and turning a moved layer keeps where it was moved to.
    let mut moved_x = IDENTITY_3D;
    moved_x[12] = 10.0;
    (want[12], want[13]) = (10.0, 0.0);
    assert_matrix(&transform_3d("CATransform3DRotate", moved_x, &[quarter, 0.0, 0.0, 1.0]), want, "rotate after translate");
    let mut shifted = IDENTITY_3D;
    (shifted[12], shifted[13], shifted[14]) = (1.0, 2.0, 3.0);
    let scaled = transform_3d("CATransform3DScale", shifted, &[2.0, 3.0, 4.0]);
    assert_matrix(&scaled, [2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 4.0, 0.0, 1.0, 2.0, 3.0, 1.0], "scale keeps the translation");
    let mut big = IDENTITY_3D;
    (big[0], big[5], big[10]) = (2.0, 3.0, 4.0);
    let translated = transform_3d("CATransform3DTranslate", big, &[1.0, 1.0, 1.0]);
    assert_matrix(&translated, [2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 4.0, 0.0, 2.0, 3.0, 4.0, 1.0], "translate is scaled");
    // CATransform3DMakeAffineTransform(&result, CGAffineTransform): six floats in r1-r3 and the stack.
    let out = alloc(64);
    call("CATransform3DMakeAffineTransform", [out, f(1.0), f(2.0), f(3.0)], &[f(4.0), f(5.0), f(6.0)]);
    assert_matrix(&floats(out, 16), [1.0, 2.0, 0.0, 0.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 5.0, 6.0, 0.0, 1.0], "affine to 3D");
    // CGRectIntersectsRect: rectangles that only touch don't intersect.
    let rect = |x: f32, w: f32| [f(x), f(0.0), f(w), f(10.0)];
    let (a, b, c) = (rect(0.0, 10.0), rect(5.0, 10.0), rect(10.0, 10.0));
    assert_eq!(call("CGRectIntersectsRect", [a[0], a[1], a[2], a[3]], &b).0, 1);
    assert_eq!(call("CGRectIntersectsRect", [a[0], a[1], a[2], a[3]], &c).0, 0);
}

// ----- Core Graphics pictures -----

/// A guest provider over a copy of `bytes` (no release callback).
fn provider_with(bytes: &[u8]) -> u32 {
    let data = alloc(bytes.len() as u32);
    mem().write_bytes(data, bytes);
    call("CGDataProviderCreateWithData", [0, data, bytes.len() as u32, 0], &[]).0
}

/// A width x 1 RGBA (premultiplied last) picture of `bytes`.
fn picture(bytes: &[u8]) -> u32 {
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let width = bytes.len() as u32 / 4;
    call("CGImageCreate", [width, 1, 8, 32], &[width * 4, space, 1, provider_with(bytes), 0, 0, 0]).0
}

/// A width x 1 RGBA context over zeroed guest pixels: (context, pixels).
fn row_context(width: u32) -> (u32, u32) {
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let pixels = alloc(width * 4);
    unsafe { std::ptr::write_bytes(mem().host(pixels), 0, width as usize * 4) };
    (call("CGBitmapContextCreate", [pixels, width, 1, 8], &[width * 4, space, 1]).0, pixels)
}

fn draw(context: u32, width: u32, image: u32) {
    call("CGContextDrawImage", [context, f(0.0), f(0.0), f(width as f32)], &[f(1.0), image]);
}

#[test]
fn image_providers_give_back_the_pictures_bytes() {
    let bytes = [255, 0, 0, 255, 0, 0, 255, 255];
    let image = picture(&bytes);
    let (provider, _) = call("CGImageGetDataProvider", [image, 0, 0, 0], &[]);
    assert_ne!(provider, 0);
    let (copied, _) = call("CGDataProviderCopyData", [provider, 0, 0, 0], &[]);
    let copied_host = super::object_to_host(copied);
    assert_eq!(crate::fw::data::bytes(copied_host), bytes);
    // A provider over a CFData makes the same picture again.
    let (again, _) = call("CGDataProviderCreateWithCFData", [copied, 0, 0, 0], &[]);
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let (image2, _) = call("CGImageCreate", [2, 1, 8, 32], &[8, space, 1, again, 0, 0, 0]);
    let (context, pixels) = row_context(2);
    draw(context, 2, image2);
    assert_eq!(mem().bytes(pixels, 8), bytes);
}

#[test]
fn image_masks_hide_where_their_samples_are_one() {
    let image = picture(&[255, 0, 0, 255, 0, 0, 255, 255]);
    // An 8-bit image mask: 0 paints, 255 hides.
    let (mask, _) = call("CGImageMaskCreate", [2, 1, 8, 8], &[2, provider_with(&[0, 255]), 0, 0]);
    assert_ne!(mask, 0);
    let (masked, _) = call("CGImageCreateWithMask", [image, mask, 0, 0], &[]);
    assert_ne!(masked, 0);
    let (context, pixels) = row_context(2);
    draw(context, 2, masked);
    assert_eq!(mem().bytes(pixels, 8), [255, 0, 0, 255, 0, 0, 0, 0]);
    // A gray picture as the mask: its samples are the alpha (the other way round).
    let (gray, _) = call("CGColorSpaceCreateDeviceGray", [0; 4], &[]);
    let (gray_image, _) = call("CGImageCreate", [2, 1, 8, 8], &[2, gray, 0, provider_with(&[0, 255]), 0, 0, 0]);
    let (masked, _) = call("CGImageCreateWithMask", [image, gray_image, 0, 0], &[]);
    let (context, pixels) = row_context(2);
    draw(context, 2, masked);
    assert_eq!(mem().bytes(pixels, 8), [0, 0, 0, 0, 0, 0, 255, 255]);
    // Drawing an image mask paints the fill colour where it lets through.
    let (context, pixels) = row_context(2);
    call("CGContextSetRGBFillColor", [context, f(0.0), f(1.0), f(0.0)], &[f(1.0)]);
    draw(context, 2, mask);
    assert_eq!(mem().bytes(pixels, 8), [0, 255, 0, 255, 0, 0, 0, 0]);
    // A 1-bit mask (first pixel in the top bit) with a decode array [1 0]:
    // the samples flip, so the first pixel hides and the second paints.
    let decode = alloc(8);
    mem().write_u32(decode, f(1.0));
    mem().write_u32(decode + 4, f(0.0));
    let bits_provider = provider_with(&[0b0100_0000]);
    let (bits, _) = call("CGImageMaskCreate", [2, 1, 1, 1], &[1, bits_provider, decode, 0]);
    let (context, pixels) = row_context(2);
    call("CGContextSetRGBFillColor", [context, f(0.0), f(0.0), f(1.0)], &[f(1.0)]);
    draw(context, 2, bits);
    assert_eq!(mem().bytes(pixels, 8), [0, 0, 0, 0, 0, 0, 255, 255]);
    // The mask keeps what it was made from: its provider and packed bytes,
    // its format, its decode array, and no colour space.
    let (provider, _) = call("CGImageGetDataProvider", [bits, 0, 0, 0], &[]);
    assert_eq!(provider, bits_provider);
    let (bytes, _) = call("CGDataProviderCopyData", [provider, 0, 0, 0], &[]);
    assert_eq!(crate::fw::data::bytes(super::object_to_host(bytes)), [0b0100_0000]);
    let format: Vec<u32> = ["CGImageGetBitsPerComponent", "CGImageGetBitsPerPixel", "CGImageGetBytesPerRow", "CGImageGetColorSpace", "CGImageIsMask"]
        .iter()
        .map(|name| call(name, [bits, 0, 0, 0], &[]).0)
        .collect();
    assert_eq!(format, [1, 1, 1, 0, 1]);
    let (kept, _) = call("CGImageGetDecode", [bits, 0, 0, 0], &[]);
    assert_eq!(floats(kept, 2), [1.0, 0.0]);
    // A picture used as the mask must be DeviceGray: an RGB one gives NULL.
    let (rgb, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let (rgb_image, _) = call("CGImageCreate", [2, 1, 8, 32], &[8, rgb, 5, provider_with(&[0; 8]), 0, 0, 0]);
    assert_ne!(rgb_image, 0);
    assert_eq!(call("CGImageCreateWithMask", [image, rgb_image, 0, 0], &[]).0, 0);
}

#[test]
fn gradients_from_colour_arrays_and_points_from_dictionaries() {
    setup();
    let color = |value: f64| crate::fw::graphics::make_color([value, value, value, 1.0]);
    let colors = super::object_to_guest(crate::fw::collections::array_from(&[color(0.0), color(1.0)]));
    let (space, _) = call("CGColorSpaceCreateDeviceRGB", [0; 4], &[]);
    let locations = alloc(8);
    mem().write_u32(locations, f(0.0));
    mem().write_u32(locations + 4, f(1.0));
    let (gradient, _) = call("CGGradientCreateWithColors", [space, colors, locations, 0], &[]);
    assert_ne!(gradient, 0);
    let (context, pixels) = {
        let pixels = alloc(16);
        unsafe { std::ptr::write_bytes(mem().host(pixels), 0, 16) };
        (call("CGBitmapContextCreate", [pixels, 4, 1, 8], &[16, space, 5]).0, pixels)
    };
    call("CGContextDrawLinearGradient", [context, gradient, f(0.0), f(0.0)], &[f(4.0), f(0.0), 0]);
    let reds: Vec<u8> = (0..4).map(|i| mem().read_u8(pixels + i * 4)).collect();
    assert_eq!(reds, [32, 96, 159, 223]);
    // CGPointMakeWithDictionaryRepresentation reads "X" and "Y".
    let key = crate::fw::string::make;
    let number = crate::fw::collections::number_float;
    let dictionary = crate::fw::collections::dictionary_from(&[(key("X"), number(3.5)), (key("Y"), number(-2.0))]);
    let point = alloc(8);
    assert_eq!(call("CGPointMakeWithDictionaryRepresentation", [super::object_to_guest(dictionary), point, 0, 0], &[]).0 & 0xff, 1);
    assert_eq!(floats(point, 2), [3.5, -2.0]);
    let half = crate::fw::collections::dictionary_from(&[(key("X"), number(1.0))]);
    assert_eq!(call("CGPointMakeWithDictionaryRepresentation", [super::object_to_guest(half), point, 0, 0], &[]).0 & 0xff, 0);
}

#[test]
fn ui_rect_fill_replaces_the_pixels_with_the_fill_colour() {
    // A 2x1 point image context, then half-transparent red over opaque blue.
    call("UIGraphicsBeginImageContextWithOptions", [f(2.0), f(1.0), 0, f(1.0)], &[]);
    let (context, _) = call("UIGraphicsGetCurrentContext", [0; 4], &[]);
    call("CGContextSetRGBFillColor", [context, f(0.0), f(0.0), f(1.0)], &[f(1.0)]);
    call("UIRectFill", [f(0.0), f(0.0), f(2.0), f(1.0)], &[]);
    call("CGContextSetRGBFillColor", [context, f(1.0), f(0.0), f(0.0)], &[f(0.5)]);
    call("UIRectFill", [f(0.0), f(0.0), f(1.0), f(1.0)], &[]);
    let host = super::object_to_host(context);
    type Get = unsafe extern "C" fn(usize) -> *mut u8;
    let get = super::host_fn!("CGBitmapContextGetData": Get).unwrap();
    // BGRA (premultiplied first, little-endian): copied, not blended over the blue.
    let pixels = unsafe { std::slice::from_raw_parts(get(host), 8) }.to_vec();
    call("UIGraphicsEndImageContext", [0; 4], &[]);
    assert_eq!(pixels, [0, 0, 128, 128, 255, 0, 0, 255]);
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

// ----- Core Foundation, CommonCrypto -----

const CF_UTF8: u32 = 0x0800_0100;
const CF_ASCII: u32 = 0x0600;
const CF_UNICODE: u32 = 0x0100;

fn host_of(guest: u32) -> usize {
    super::object_to_host(guest)
}

fn guest_of(host: usize) -> u32 {
    super::object_to_guest(host)
}

#[test]
fn cf_data_bytes_live_in_guest_memory_and_follow_the_length() {
    let source = alloc(5);
    mem().write_bytes(source, b"hello");
    let one = |name: &str, data: u32| call(name, [data, 0, 0, 0], &[]).0;
    let data = call("CFDataCreate", [0, source, 5, 0], &[]).0;
    assert_eq!(one("CFDataGetLength", data), 5);
    let bytes = one("CFDataGetBytePtr", data);
    assert_ne!(bytes, source, "CFDataCreate copies");
    assert_eq!(mem().bytes(bytes, 5), b"hello");

    // A capacity keeps the pointer; the game's writes through it are the data's bytes.
    let fixed = call("CFDataCreateMutable", [0, 16, 0, 0], &[]).0;
    let start = one("CFDataGetMutableBytePtr", fixed);
    assert_ne!(start, 0);
    call("CFDataSetLength", [fixed, 8, 0, 0], &[]);
    assert_eq!(one("CFDataGetMutableBytePtr", fixed), start, "the pointer stays within the capacity");
    assert_eq!(mem().bytes(start, 8), [0; 8], "new length reads as zeros");
    mem().write_bytes(start, b"12345678");
    assert_eq!(one("CFDataGetBytePtr", fixed), start);
    assert_eq!(crate::fw::data::bytes(host_of(fixed)), b"12345678", "the host object holds what the game wrote");
    let message = crate::objc::runtime::send(host_of(fixed), "bytes", &[]);
    assert_eq!(mem().guest(message as *const u8), start, "-bytes is the same memory");

    // Growing past the capacity moves the bytes and keeps them.
    let growing = call("CFDataCreateMutable", [0, 0, 0, 0], &[]).0;
    call("CFDataSetLength", [growing, 4, 0, 0], &[]);
    mem().write_bytes(one("CFDataGetMutableBytePtr", growing), b"abcd");
    call("CFDataSetLength", [growing, 5000, 0, 0], &[]);
    let moved = one("CFDataGetBytePtr", growing);
    assert_eq!(mem().bytes(moved, 4), b"abcd");
    assert!(mem().bytes(moved + 4, 4996).iter().all(|&b| b == 0));
    assert_eq!(one("CFDataGetLength", growing), 5000);

    // No copy: the data reads the caller's own bytes.
    let borrowed = call("CFDataCreateWithBytesNoCopy", [0, source, 5, 0], &[]).0;
    assert_eq!(one("CFDataGetBytePtr", borrowed), source);
    assert_eq!(one("CFDataGetLength", borrowed), 5);
}

#[test]
fn cf_collections_and_numbers_use_32_bit_words() {
    setup();
    use crate::fw::{collections, string};
    let (a, b) = (string::make("a"), string::make("b"));
    let array = guest_of(collections::array_from(&[a, b, a]));
    assert_eq!(call("CFArrayGetCount", [array, 0, 0, 0], &[]).0, 3);
    // CFArrayGetValues(array, CFRange {1, 2}, values): two 4-byte words, nothing past them.
    let values = alloc(12);
    mem().write_u32(values + 8, 0xDEAD_BEEF);
    call("CFArrayGetValues", [array, 1, 2, values], &[]);
    assert_eq!([mem().read_u32(values), mem().read_u32(values + 4), mem().read_u32(values + 8)], [guest_of(b), guest_of(a), 0xDEAD_BEEF]);

    let dictionary = guest_of(collections::dictionary_from(&[(a, b)]));
    assert_eq!(call("CFDictionaryGetCount", [dictionary, 0, 0, 0], &[]).0, 1);
    let (keys, objects) = (alloc(8), alloc(8));
    mem().write_u32(keys + 4, 0xDEAD_BEEF);
    call("CFDictionaryGetKeysAndValues", [dictionary, keys, objects, 0], &[]);
    assert_eq!([mem().read_u32(keys), mem().read_u32(objects), mem().read_u32(keys + 4)], [guest_of(a), guest_of(b), 0xDEAD_BEEF]);
    call("CFDictionaryGetKeysAndValues", [dictionary, keys, 0, 0], &[]);

    // kCFNumberLongType (10) is 4 bytes here and kCFNumberCGFloatType (16) a float.
    let value = alloc(8);
    mem().write_u32(value, -7i32 as u32);
    mem().write_u32(value + 4, 0x7FFF_FFFF);
    let long = call("CFNumberCreate", [0, 10, value, 0], &[]).0;
    assert_eq!(collections::number_as_i64(host_of(long)), -7);
    mem().write_u32(value, 2.5f32.to_bits());
    let float = call("CFNumberCreate", [0, 16, value, 0], &[]).0;
    assert_eq!(collections::number_as_f64(host_of(float)), 2.5);

    let other_a = guest_of(string::make("a"));
    assert_eq!(call("CFEqual", [guest_of(a), other_a, 0, 0], &[]).0, 1);
    assert_eq!(call("CFEqual", [guest_of(a), guest_of(b), 0, 0], &[]).0, 0);
    assert_eq!(call("CFHash", [guest_of(a), 0, 0, 0], &[]).0, call("CFHash", [other_a, 0, 0, 0], &[]).0);
}

#[test]
fn cf_string_bytes_use_32_bit_ranges_and_counts() {
    setup();
    let text = guest_of(crate::fw::string::make("h\u{e9}llo"));
    assert_eq!(call("CFStringGetLength", [text, 0, 0, 0], &[]).0, 5);
    let (buffer, used) = (alloc(16), alloc(8));
    mem().write_u32(used + 4, 0xDEAD_BEEF);
    // CFStringGetBytes(string, CFRange {1, 3}, UTF-8, loss byte, external, buffer, room, &used)
    let converted = call("CFStringGetBytes", [text, 1, 3, CF_UTF8], &[0, 0, buffer, 16, used]).0;
    assert_eq!((converted, mem().read_u32(used)), (3, 4));
    assert_eq!(mem().bytes(buffer, 4), "\u{e9}ll".as_bytes());
    assert_eq!(mem().read_u32(used + 4), 0xDEAD_BEEF, "a 4-byte CFIndex was written");
    let converted = call("CFStringGetBytes", [text, 0, 5, CF_ASCII], &[b'?' as u32, 0, buffer, 16, used]).0;
    assert_eq!((converted, mem().bytes(buffer, 5)), (5, &b"h?llo"[..]));
    // CFStringCreateWithBytes(allocator, bytes, count, encoding, external representation)
    mem().write_bytes(buffer, &[0xFF, 0xFE, b'h', 0, b'i', 0]);
    let made = call("CFStringCreateWithBytes", [0, buffer, 6, CF_UNICODE], &[1]).0;
    assert_eq!(crate::fw::string::get(host_of(made)), "hi");
    assert_eq!(call("CFStringGetMaximumSizeForEncoding", [5, CF_UTF8, 0, 0], &[]).0, 15);
}

#[test]
fn digest_contexts_stay_inside_apples_sizes() {
    use crate::fw::crypto::{md5, sha1, sha256};
    let data = alloc(3);
    mem().write_bytes(data, b"abc");
    let kinds: [(&str, u32, Vec<u8>); 3] =
        [("CC_MD5", 92, md5(b"abc").to_vec()), ("CC_SHA1", 96, sha1(b"abc").to_vec()), ("CC_SHA256", 104, sha256(b"abc").to_vec())];
    for (prefix, size, expected) in kinds {
        let context = alloc(size + 4);
        mem().write_u32(context + size, 0xDEAD_BEEF);
        assert_eq!(call(&format!("{prefix}_Init"), [context, 0, 0, 0], &[]).0, 1);
        call(&format!("{prefix}_Update"), [context, data, 1, 0], &[]);
        call(&format!("{prefix}_Update"), [context, data + 1, 2, 0], &[]);
        let out = alloc(32);
        assert_eq!(call(&format!("{prefix}_Final"), [out, context, 0, 0], &[]).0, 1);
        assert_eq!(mem().bytes(out, expected.len()), &expected[..], "{prefix}");
        assert_eq!(mem().read_u32(context + size), 0xDEAD_BEEF, "{prefix} wrote past its {size}-byte context");
    }
}

#[test]
fn foundation_constants_have_32_bit_forms() {
    setup();
    let word = |name: &str| mem().read_u32(crate::guest32::data_address(name).unwrap_or_else(|| panic!("{name}")));
    assert_eq!(word("vm_page_size"), 4096);
    assert_eq!(word("OBJC_IVAR_$_NSObject.isa"), 0);
    let ndr = crate::guest32::data_address("NDR_record").unwrap();
    assert_eq!(mem().bytes(ndr, 8), [0, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(f32::from_bits(word("UIWindowLevelStatusBar")), 1000.0);
    let object = |name: &str| host_of(word(name));
    assert_eq!(crate::fw::string::get(object("kCAMediaTimingFunctionEaseInEaseOut")), "easeInEaseOut");
    assert_eq!(crate::fw::string::get(object("kSecClassKey")), "keys");
    assert!(crate::fw::collections::number_as_f64(object("kCFNumberNaN")).is_nan());
    assert_eq!(object("kCFNull"), crate::objc::runtime::send(crate::objc::runtime::class_named("NSNull"), "null", &[]));
    assert_eq!(crate::fw::string::get(object("NSFileModificationDate")), "NSFileModificationDate");
    let version = crate::guest32::data_address("kCFCoreFoundationVersionNumber").expect("kCFCoreFoundationVersionNumber");
    assert_eq!(f64::from_bits(mem().read_u64(version)), 793.0, "iOS 6.1's CoreFoundation version");
}

#[test]
fn indexed_gl_queries_reach_the_guest_registry() {
    setup();
    for name in ["glGetIntegeri_v", "glGetInteger64i_v", "glGetBooleani_v"] {
        assert!(ios32::provides_function(name), "{name} is registered");
    }
}

#[test]
fn address_book_records_and_multivalues_round_trip() {
    setup();
    let word = |name: &str| mem().read_u32(crate::guest32::data_address(name).unwrap_or_else(|| panic!("{name}")));
    assert_eq!(word("kABPersonFirstNameProperty"), 0);
    assert_eq!(word("kABPersonLastNameProperty"), 1);
    assert_eq!(word("kABPersonEmailProperty"), 4);
    assert_eq!(word("kABPersonPhoneProperty"), 3);
    assert_eq!(crate::fw::string::get(host_of(word("kABWorkLabel"))), "_$!<Work>!$_");

    // An address book with no people in it.
    let book = call("ABAddressBookCreate", [0; 4], &[]).0;
    assert_ne!(book, 0);
    let people = call("ABAddressBookCopyArrayOfAllPeople", [book, 0, 0, 0], &[]).0;
    assert_ne!(people, 0);
    assert_eq!(call("CFArrayGetCount", [people, 0, 0, 0], &[]).0, 0);
    assert_eq!(call("CFArrayGetValueAtIndex", [people, 0, 0, 0], &[]).0, 0);
    // CFArrayGetValueAtIndex hands back the item itself (array objects are NSArray here).
    let list = guest_of(crate::fw::collections::array_from(&[crate::fw::string::make("one")]));
    assert_eq!(crate::fw::string::get(host_of(call("CFArrayGetValueAtIndex", [list, 0, 0, 0], &[]).0)), "one");
    assert_eq!(call("CFArrayGetValueAtIndex", [list, 1, 0, 0], &[]).0, 0, "past the end is NULL");

    // Record values are stored per property, replaced on a second set, and read back +1.
    let person = call("ABPersonCreate", [0; 4], &[]).0;
    assert_ne!(person, 0);
    let (first, last) = (word("kABPersonFirstNameProperty"), word("kABPersonLastNameProperty"));
    let grace = guest_of(crate::fw::string::make("Grace"));
    let hopper = guest_of(crate::fw::string::make("Hopper"));
    assert_eq!(call("ABRecordSetValue", [person, first, grace, 0], &[]).0, 1);
    assert_eq!(call("ABRecordSetValue", [person, last, hopper, 0], &[]).0, 1);
    assert_eq!(call("ABRecordCopyValue", [person, first, 0, 0], &[]).0, grace);
    assert_eq!(crate::fw::string::get(host_of(call("ABRecordCopyValue", [person, last, 0, 0], &[]).0)), "Hopper");
    assert_eq!(call("ABRecordCopyValue", [person, word("kABPersonEmailProperty"), 0, 0], &[]).0, 0);
    let admiral = guest_of(crate::fw::string::make("Admiral"));
    assert_eq!(call("ABRecordSetValue", [person, first, admiral, 0], &[]).0, 1);
    assert_eq!(crate::fw::string::get(host_of(call("ABRecordCopyValue", [person, first, 0, 0], &[]).0)), "Admiral");
    assert_eq!(call("ABPersonGetSortOrdering", [0; 4], &[]).0, 0, "kABPersonSortByFirstName");

    // A multi value keeps its (value, label) pairs in order; identifiers are their indexes.
    let phones = call("ABMultiValueCreateMutable", [word("kABPersonPhoneProperty"), 0, 0, 0], &[]).0;
    assert_ne!(phones, 0);
    let work = guest_of(crate::fw::string::make("_$!<Work>!$_"));
    let home = guest_of(crate::fw::string::make("_$!<Home>!$_"));
    let number1 = guest_of(crate::fw::string::make("+1 555 0100"));
    let number2 = guest_of(crate::fw::string::make("+1 555 0199"));
    let identifier = alloc(4);
    mem().write_u32(identifier, 0xDEAD_BEEF);
    assert_eq!(call("ABMultiValueAddValueAndLabel", [phones, number1, work, identifier], &[]).0, 1);
    assert_eq!(mem().read_u32(identifier), 0);
    assert_eq!(call("ABMultiValueAddValueAndLabel", [phones, number2, home, 0], &[]).0, 1);
    assert_eq!(call("ABMultiValueGetCount", [phones, 0, 0, 0], &[]).0, 2);
    assert_eq!(call("ABMultiValueCopyValueAtIndex", [phones, 1, 0, 0], &[]).0, number2);
    assert_eq!(crate::fw::string::get(host_of(call("ABMultiValueCopyValueAtIndex", [phones, 0, 0, 0], &[]).0)), "+1 555 0100");
    assert_eq!(call("ABMultiValueCopyValueAtIndex", [phones, 2, 0, 0], &[]).0, 0);

    // The comparator: first then last name, or last then first with ordering 1.
    let other = call("ABPersonCreate", [0; 4], &[]).0;
    call("ABRecordSetValue", [other, first, guest_of(crate::fw::string::make("Ada")), 0], &[]);
    assert_eq!(call("ABPersonComparePeopleByName", [other, person, 0, 0], &[]).0 as i32, -1);
    assert_eq!(call("ABPersonComparePeopleByName", [person, other, 0, 0], &[]).0 as i32, 1);
    assert_eq!(call("ABPersonComparePeopleByName", [person, person, 0, 0], &[]).0 as i32, 0);
    assert_eq!(call("ABPersonComparePeopleByName", [other, person, 1, 0], &[]).0 as i32, -1);
    // Taken as data (a pointer bind), the loader resolves the function stand-in.
    assert!(ios32::provides_function("ABPersonComparePeopleByName"));
}

#[test]
fn the_bundle_url_path_is_the_bundle_path() {
    setup();
    if crate::loader::try_app_dir().is_none() {
        // An existing folder, so other tests can put their pictures there too.
        let folder = std::env::temp_dir().to_string_lossy().into_owned();
        crate::loader::set_guest32_app("", &folder);
    }
    let bundle = call("CFBundleGetMainBundle", [0; 4], &[]).0;
    assert_ne!(bundle, 0);
    assert_eq!(
        bundle,
        guest_of(crate::objc::runtime::send(crate::objc::runtime::class_named("NSBundle"), "mainBundle", &[])),
        "the same object as +[NSBundle mainBundle]"
    );
    let url = call("CFBundleCopyBundleURL", [bundle, 0, 0, 0], &[]).0;
    assert_ne!(url, 0);
    let path = call("CFURLCopyFileSystemPath", [url, 0, 0, 0], &[]).0;
    assert_eq!(crate::fw::string::get(host_of(path)), crate::loader::app_dir());
    // A non-file URL has no file system path.
    let remote = guest_of(crate::objc::runtime::send(
        crate::objc::runtime::class_named("NSURL"),
        "URLWithString:",
        &[crate::fw::string::make("http://example.com/a")],
    ));
    assert_eq!(call("CFURLCopyFileSystemPath", [remote, 0, 0, 0], &[]).0, 0);
    // NSHomeDirectory is the same string the host already hands to 64-bit games.
    let home = call("NSHomeDirectory", [0; 4], &[]).0;
    assert_eq!(crate::fw::string::get(host_of(home)), crate::fw::foundation::home());
}

#[test]
fn cf_run_loop_run_in_mode_takes_the_guest_double_and_times_out() {
    setup();
    let mode = guest_of(crate::fw::string::make("kCFRunLoopDefaultMode"));
    let bits = 0.1f64.to_bits();
    let start = std::time::Instant::now();
    let (result, _) = call("CFRunLoopRunInMode", [mode, bits as u32, (bits >> 32) as u32, 0], &[]);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(result, 3, "kCFRunLoopRunTimedOut");
    assert!(elapsed >= 0.08, "the timeout double arrived ({elapsed}s)");
    assert!(elapsed < 0.5, "and was not misread");
}

#[test]
fn class_responds_to_selector_answers_for_framework_classes() {
    setup();
    let _ = crate::guest32::objc::wire::test_bridge();
    let selector = |name: &str| call("sel_registerName", [cstring(name), 0, 0, 0], &[]).0;
    let class = call("objc_getClass", [cstring("NSString"), 0, 0, 0], &[]).0;
    assert_ne!(class, 0);
    assert_eq!(call("class_respondsToSelector", [class, selector("length"), 0, 0], &[]).0, 1);
    assert_eq!(call("class_respondsToSelector", [class, selector("noSuchSelectorAnywhere"), 0, 0], &[]).0, 0);
    assert_eq!(call("class_respondsToSelector", [0, selector("length"), 0, 0], &[]).0, 0);
}
