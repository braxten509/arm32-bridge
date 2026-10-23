//! Core Graphics, Core Animation's C functions and UIKit's C functions for
//! 32-bit games: this layer's drawing (fw/graphics.rs, fw/geometry.rs) with
//! CGFloats, structs, CGFloat arrays and transform pointers converted.

use super::{
    host_fn, host_functions, object_to_guest, object_to_host, register_all, AffinePtr, Bool, ColorPtr, Float, Obj, Ptr,
    Word, I32, U32,
};
use crate::fw::geometry::{Point, Rect, Size, Transform, Transform3D};
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};
use std::collections::HashMap;
use std::sync::Mutex;

fn function(name: &str, handler: Option<arm32_bridge::runtime::Handler>) {
    super::super::function(name, handler.expect(name));
}

/// `count` guest CGFloats as the host's doubles.
fn guest_floats(address: u32, count: usize) -> Vec<f64> {
    (0..count as u32).map(|i| f32::from_bits(mem().read_u32(address + i * 4)) as f64).collect()
}

pub fn register() {
    register_all(host_functions! {
        // geometry
        "CGRectGetWidth" => fn(r: Rect) -> Float;
        "CGRectGetHeight" => fn(r: Rect) -> Float;
        "CGRectGetMinX" => fn(r: Rect) -> Float;
        "CGRectGetMinY" => fn(r: Rect) -> Float;
        "CGRectGetMaxX" => fn(r: Rect) -> Float;
        "CGRectGetMaxY" => fn(r: Rect) -> Float;
        "CGRectGetMidX" => fn(r: Rect) -> Float;
        "CGRectGetMidY" => fn(r: Rect) -> Float;
        "CGRectContainsPoint" => fn(r: Rect, p: Point) -> Bool;
        "CGRectInset" => fn(r: Rect, dx: Float, dy: Float) -> Rect;
        "CGRectOffset" => fn(r: Rect, dx: Float, dy: Float) -> Rect;
        "CGRectEqualToRect" => fn(a: Rect, b: Rect) -> Bool;
        "CGRectIsEmpty" => fn(r: Rect) -> Bool;
        "CGRectIsNull" => fn(r: Rect) -> Bool;
        "CGRectUnion" => fn(a: Rect, b: Rect) -> Rect;
        "CGRectMake" => fn(x: Float, y: Float, w: Float, h: Float) -> Rect;
        "CGRectApplyAffineTransform" => fn(r: Rect, t: Transform) -> Rect;
        "CGPointApplyAffineTransform" => fn(p: Point, t: Transform) -> Point;
        "CGSizeApplyAffineTransform" => fn(s: Size, t: Transform) -> Size;
        "CGAffineTransformMakeRotation" => fn(angle: Float) -> Transform;
        "CGAffineTransformMakeScale" => fn(sx: Float, sy: Float) -> Transform;
        "CGAffineTransformMakeTranslation" => fn(tx: Float, ty: Float) -> Transform;
        "CGAffineTransformRotate" => fn(t: Transform, angle: Float) -> Transform;
        "CGAffineTransformScale" => fn(t: Transform, sx: Float, sy: Float) -> Transform;
        "CGAffineTransformTranslate" => fn(t: Transform, tx: Float, ty: Float) -> Transform;
        "CGAffineTransformConcat" => fn(a: Transform, b: Transform) -> Transform;
        "CGAffineTransformIsIdentity" => fn(t: Transform) -> Bool;
        "CGAffineTransformInvert" => fn(t: Transform) -> Transform;
        "CATransform3DMakeScale" => fn(sx: Float, sy: Float, sz: Float) -> Transform3D;
        "CATransform3DMakeTranslation" => fn(tx: Float, ty: Float, tz: Float) -> Transform3D;
        "CGPointFromString" => fn(text: Obj) -> Point;
        "CGSizeFromString" => fn(text: Obj) -> Size;
        "CGRectFromString" => fn(text: Obj) -> Rect;
        "NSStringFromCGPoint" => fn(p: Point) -> Obj;
        "NSStringFromCGSize" => fn(s: Size) -> Obj;
        "NSStringFromCGRect" => fn(r: Rect) -> Obj;
        // images, colour spaces, colours, gradients
        "CGImageGetWidth" => fn(image: Obj) -> Word;
        "CGImageGetHeight" => fn(image: Obj) -> Word;
        "CGImageGetBitsPerComponent" => fn(image: Obj) -> Word;
        "CGImageGetBitsPerPixel" => fn(image: Obj) -> Word;
        "CGImageGetBytesPerRow" => fn(image: Obj) -> Word;
        "CGImageGetBitmapInfo" => fn(image: Obj) -> U32;
        "CGImageGetAlphaInfo" => fn(image: Obj) -> U32;
        "CGImageGetColorSpace" => fn(image: Obj) -> Obj;
        "CGImageRetain" => fn(image: Obj) -> Obj;
        "CGImageRelease" => fn(image: Obj);
        "CGImageCreate" => fn(width: Word, height: Word, bpc: Word, bpp: Word, bpr: Word, space: Obj, info: U32, provider: Obj, decode: Ptr, interpolate: Bool, intent: I32) -> Obj;
        "CGContextRetain" => fn(context: Obj) -> Obj;
        "CGColorSpaceCreateDeviceRGB" => fn() -> Obj;
        "CGColorSpaceCreateDeviceGray" => fn() -> Obj;
        "CGColorSpaceGetNumberOfComponents" => fn(space: Obj) -> Word;
        "CGColorSpaceRelease" => fn(space: Obj);
        "CGColorCreate" => fn(space: Obj, components: ColorPtr) -> Obj;
        "CGColorRelease" => fn(color: Obj);
        "CGColorGetNumberOfComponents" => fn(color: Obj) -> Word;
        "CGColorGetAlpha" => fn(color: Obj) -> Float;
        "CGGradientRelease" => fn(gradient: Obj);
        "CGDataProviderRetain" => fn(provider: Obj) -> Obj;
        // bitmap contexts and drawing
        "CGBitmapContextCreateImage" => fn(context: Obj) -> Obj;
        "CGBitmapContextGetWidth" => fn(context: Obj) -> Word;
        "CGBitmapContextGetHeight" => fn(context: Obj) -> Word;
        "CGBitmapContextGetBytesPerRow" => fn(context: Obj) -> Word;
        "CGBitmapContextGetBitsPerComponent" => fn(context: Obj) -> Word;
        "CGBitmapContextGetBitsPerPixel" => fn(context: Obj) -> Word;
        "CGBitmapContextGetBitmapInfo" => fn(context: Obj) -> U32;
        "CGBitmapContextGetColorSpace" => fn(context: Obj) -> Obj;
        "CGContextConcatCTM" => fn(context: Obj, t: Transform);
        "CGContextTranslateCTM" => fn(context: Obj, tx: Float, ty: Float);
        "CGContextScaleCTM" => fn(context: Obj, sx: Float, sy: Float);
        "CGContextRotateCTM" => fn(context: Obj, angle: Float);
        "CGContextGetCTM" => fn(context: Obj) -> Transform;
        "CGContextSaveGState" => fn(context: Obj);
        "CGContextRestoreGState" => fn(context: Obj);
        "CGContextSetFillColorWithColor" => fn(context: Obj, color: Obj);
        "CGContextSetStrokeColorWithColor" => fn(context: Obj, color: Obj);
        "CGContextSetRGBFillColor" => fn(context: Obj, r: Float, g: Float, b: Float, a: Float);
        "CGContextSetRGBStrokeColor" => fn(context: Obj, r: Float, g: Float, b: Float, a: Float);
        "CGContextSetGrayFillColor" => fn(context: Obj, gray: Float, alpha: Float);
        "CGContextSetGrayStrokeColor" => fn(context: Obj, gray: Float, alpha: Float);
        "CGContextSetFillColor" => fn(context: Obj, components: ColorPtr);
        "CGContextSetStrokeColor" => fn(context: Obj, components: ColorPtr);
        "CGContextSetFillColorSpace" => fn(context: Obj, space: Obj);
        "CGContextSetStrokeColorSpace" => fn(context: Obj, space: Obj);
        "CGContextSetAlpha" => fn(context: Obj, alpha: Float);
        "CGContextSetLineWidth" => fn(context: Obj, width: Float);
        "CGContextSetBlendMode" => fn(context: Obj, mode: I32);
        "CGContextSetShadow" => fn(context: Obj, offset: Size, blur: Float);
        "CGContextSetShadowWithColor" => fn(context: Obj, offset: Size, blur: Float, color: Obj);
        "CGContextSetTextDrawingMode" => fn(context: Obj, mode: I32);
        "CGContextBeginTransparencyLayer" => fn(context: Obj, auxiliary: Obj);
        "CGContextBeginTransparencyLayerWithRect" => fn(context: Obj, rect: Rect, auxiliary: Obj);
        "CGContextEndTransparencyLayer" => fn(context: Obj);
        "CGContextFillRect" => fn(context: Obj, rect: Rect);
        "CGContextClearRect" => fn(context: Obj, rect: Rect);
        "CGContextStrokeRect" => fn(context: Obj, rect: Rect);
        "CGContextFillEllipseInRect" => fn(context: Obj, rect: Rect);
        "CGContextStrokeEllipseInRect" => fn(context: Obj, rect: Rect);
        "CGContextDrawImage" => fn(context: Obj, rect: Rect, image: Obj);
        "CGContextDrawLinearGradient" => fn(context: Obj, gradient: Obj, start: Point, end: Point, options: U32);
        "CGContextDrawRadialGradient" => fn(context: Obj, gradient: Obj, start: Point, start_radius: Float, end: Point, end_radius: Float, options: U32);
        "CGContextBeginPath" => fn(context: Obj);
        "CGContextMoveToPoint" => fn(context: Obj, x: Float, y: Float);
        "CGContextAddLineToPoint" => fn(context: Obj, x: Float, y: Float);
        "CGContextClosePath" => fn(context: Obj);
        "CGContextAddRect" => fn(context: Obj, rect: Rect);
        "CGContextAddEllipseInRect" => fn(context: Obj, rect: Rect);
        "CGContextAddArc" => fn(context: Obj, x: Float, y: Float, radius: Float, start: Float, end: Float, clockwise: I32);
        "CGContextAddArcToPoint" => fn(context: Obj, x1: Float, y1: Float, x2: Float, y2: Float, radius: Float);
        "CGContextAddPath" => fn(context: Obj, path: Obj);
        "CGContextDrawPath" => fn(context: Obj, mode: I32);
        "CGContextFillPath" => fn(context: Obj);
        "CGContextEOFillPath" => fn(context: Obj);
        "CGContextStrokePath" => fn(context: Obj);
        "CGContextClip" => fn(context: Obj);
        "CGContextEOClip" => fn(context: Obj);
        "CGContextClipToRect" => fn(context: Obj, rect: Rect);
        // paths
        "CGPathCreateMutable" => fn() -> Obj;
        "CGPathRetain" => fn(path: Obj) -> Obj;
        "CGPathRelease" => fn(path: Obj);
        "CGPathMoveToPoint" => fn(path: Obj, m: AffinePtr, x: Float, y: Float);
        "CGPathAddLineToPoint" => fn(path: Obj, m: AffinePtr, x: Float, y: Float);
        "CGPathAddArcToPoint" => fn(path: Obj, m: AffinePtr, x1: Float, y1: Float, x2: Float, y2: Float, radius: Float);
        "CGPathAddArc" => fn(path: Obj, m: AffinePtr, x: Float, y: Float, radius: Float, start: Float, end: Float, clockwise: Bool);
        "CGPathAddRect" => fn(path: Obj, m: AffinePtr, rect: Rect);
        "CGPathAddEllipseInRect" => fn(path: Obj, m: AffinePtr, rect: Rect);
        "CGPathCloseSubpath" => fn(path: Obj);
        // UIKit
        "UIGraphicsBeginImageContext" => fn(size: Size);
        "UIGraphicsBeginImageContextWithOptions" => fn(size: Size, opaque: Bool, scale: Float);
        "UIGraphicsGetCurrentContext" => fn() -> Obj;
        "UIGraphicsPushContext" => fn(context: Obj);
        "UIGraphicsPopContext" => fn();
        "UIGraphicsGetImageFromCurrentImageContext" => fn() -> Obj;
        "UIGraphicsEndImageContext" => fn();
        "UIImagePNGRepresentation" => fn(image: Obj) -> Obj;
        "UIImageJPEGRepresentation" => fn(image: Obj, quality: Float) -> Obj;
    });
    special();
    for name in ["CGRectZero", "CGRectNull"] {
        super::struct_constant::<Rect>(name);
    }
    super::struct_constant::<Point>("CGPointZero");
    super::struct_constant::<Size>("CGSizeZero");
    super::struct_constant::<Transform>("CGAffineTransformIdentity");
    super::struct_constant::<Transform3D>("CATransform3DIdentity");
}

/// Guest pixel memory of bitmap contexts the game made without its own (freed with the context).
static OWN_PIXELS: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);
/// Guest copies of colours' components (CGColorGetComponents), per host colour.
static COMPONENTS: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);
/// Guest release callbacks of data providers: (callback, info, data, size).
static RELEASES: Mutex<Option<HashMap<usize, (u32, u32, u32, u32)>>> = Mutex::new(None);

fn special() {
    // CGBitmapContextCreate(data, width, height, bitsPerComponent, bytesPerRow, space, info):
    // with no memory given, the pixels go in guest memory (the game may read them).
    function(
        "CGBitmapContextCreate",
        handler!(|uc, a| {
            let (data, width, height, bpc, bpr, space, info) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            type Create = unsafe extern "C" fn(*mut u8, usize, usize, usize, usize, usize, u32) -> usize;
            let Some(create) = host_fn!("CGBitmapContextCreate": Create) else { return super::no_host_function(uc, "CGBitmapContextCreate") };
            let space = object_to_host(space);
            let (width, height, bpc) = (width as usize, height as usize, bpc as usize);
            if data != 0 {
                return Ret::U32(object_to_guest(unsafe { create(mem().host(data), width, height, bpc, bpr as usize, space, info) }));
            }
            // The host's own choice of row size, then the same context over guest memory.
            let probe = unsafe { create(std::ptr::null_mut(), width, height, bpc, bpr as usize, space, info) };
            if probe == 0 {
                return Ret::U32(0);
            }
            type Get = unsafe extern "C" fn(usize) -> usize;
            let row = host_fn!("CGBitmapContextGetBytesPerRow": Get).map_or(0, |get| unsafe { get(probe) });
            if let Some(release) = host_fn!("CGContextRelease": unsafe extern "C" fn(usize)) {
                unsafe { release(probe) };
            }
            let pixels = crate::guest32::alloc((row * height) as u32);
            unsafe { std::ptr::write_bytes(mem().host(pixels), 0, row * height) };
            let context = unsafe { create(mem().host(pixels), width, height, bpc, row, space, info) };
            if context == 0 {
                crate::guest32::free(pixels);
                return Ret::U32(0);
            }
            OWN_PIXELS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(context, pixels);
            Ret::U32(object_to_guest(context))
        }),
    );
    function(
        "CGContextRelease",
        handler!(|uc, a| {
            let context = object_to_host(a.u32());
            if context == 0 {
                return Ret::Void;
            }
            if let Some(release) = host_fn!("CGContextRelease": unsafe extern "C" fn(usize)) {
                unsafe { release(context) };
            }
            // A context that is gone has no memory any more.
            let gone = host_fn!("CGBitmapContextGetData": unsafe extern "C" fn(usize) -> *mut u8).is_some_and(|get| unsafe { get(context) }.is_null());
            if gone {
                if let Some(pixels) = OWN_PIXELS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&context)) {
                    crate::guest32::free(pixels);
                }
            }
            Ret::Void
        }),
    );
    function(
        "CGBitmapContextGetData",
        handler!(|uc, a| {
            let context = object_to_host(a.u32());
            let data = host_fn!("CGBitmapContextGetData": unsafe extern "C" fn(usize) -> *mut u8).map_or(std::ptr::null_mut(), |get| unsafe { get(context) });
            Ret::U32(guest_address(data as usize).unwrap_or_else(|| {
                if !data.is_null() {
                    crate::log!("CGBitmapContextGetData: the context's pixels are outside guest memory");
                }
                0
            }))
        }),
    );
    // CGGradientCreateWithColorComponents(space, components, locations, count).
    function(
        "CGGradientCreateWithColorComponents",
        handler!(|uc, a| {
            let (space, components, locations, count) = (object_to_host(a.u32()), a.u32(), a.u32(), a.u32() as usize);
            type Count = unsafe extern "C" fn(usize) -> usize;
            type Create = unsafe extern "C" fn(usize, *const f64, *const f64, usize) -> usize;
            let per = host_fn!("CGColorSpaceGetNumberOfComponents": Count).map_or(0, |n| unsafe { n(space) }) + 1;
            let Some(create) = host_fn!("CGGradientCreateWithColorComponents": Create) else { return super::no_host_function(uc, "CGGradientCreateWithColorComponents") };
            if components == 0 || per < 2 || count == 0 {
                return Ret::U32(0);
            }
            let values = guest_floats(components, per * count);
            let stops = (locations != 0).then(|| guest_floats(locations, count));
            let gradient = unsafe { create(space, values.as_ptr(), stops.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()), count) };
            Ret::U32(object_to_guest(gradient))
        }),
    );
    // CGColorGetComponents: a guest copy as floats, kept per colour.
    function(
        "CGColorGetComponents",
        handler!(|uc, a| {
            let color = object_to_host(a.u32());
            type Get = unsafe extern "C" fn(usize) -> *const f64;
            type Count = unsafe extern "C" fn(usize) -> usize;
            let values = host_fn!("CGColorGetComponents": Get).map_or(std::ptr::null(), |get| unsafe { get(color) });
            let count = host_fn!("CGColorGetNumberOfComponents": Count).map_or(0, |n| unsafe { n(color) });
            if values.is_null() || count == 0 {
                return Ret::U32(0);
            }
            let guest = *COMPONENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(color).or_insert_with(|| crate::guest32::alloc_permanent(count as u32 * 4, 4));
            for i in 0..count {
                mem().write_u32(guest + i as u32 * 4, (unsafe { *values.add(i) } as f32).to_bits());
            }
            Ret::U32(guest)
        }),
    );
    // CGContextStrokeLineSegments(context, points, count).
    function(
        "CGContextStrokeLineSegments",
        handler!(|uc, a| {
            let (context, points, count) = (object_to_host(a.u32()), a.u32(), a.u32() as usize);
            let Some(stroke) = host_fn!("CGContextStrokeLineSegments": unsafe extern "C" fn(usize, *const Point, usize)) else {
                return super::no_host_function(uc, "CGContextStrokeLineSegments");
            };
            let values = guest_floats(points, count * 2);
            let host: Vec<Point> = values.chunks_exact(2).map(|p| Point { x: p[0], y: p[1] }).collect();
            unsafe { stroke(context, host.as_ptr(), host.len()) };
            Ret::Void
        }),
    );
    // CGDataProviderCreateWithData(info, data, size, release): the release
    // callback is guest code, called here when the game releases the
    // provider (as the host does: CGImageCreate copies the bytes).
    function(
        "CGDataProviderCreateWithData",
        handler!(|uc, a| {
            let (info, data, size, release) = (a.u32(), a.u32(), a.u32(), a.u32());
            type Create = unsafe extern "C" fn(usize, *const u8, usize, usize) -> usize;
            let Some(create) = host_fn!("CGDataProviderCreateWithData": Create) else { return super::no_host_function(uc, "CGDataProviderCreateWithData") };
            let provider = unsafe { create(info as usize, mem().host_or_null(data), size as usize, 0) };
            if provider != 0 && release != 0 {
                RELEASES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(provider, (release, info, data, size));
            }
            Ret::U32(object_to_guest(provider))
        }),
    );
    function(
        "CGDataProviderRelease",
        handler!(|uc, a| {
            let provider = object_to_host(a.u32());
            if provider == 0 {
                return Ret::Void;
            }
            if let Some(release) = host_fn!("CGDataProviderRelease": unsafe extern "C" fn(usize)) {
                unsafe { release(provider) };
            }
            let callback = RELEASES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&provider));
            if let Some((function, info, data, size)) = callback {
                if let Err(error) = crate::guest32::call(uc, function, &[info, data, size]) {
                    arm32_bridge::runtime::stop_with_fault(uc, format!("CGDataProvider release callback: {error}"));
                }
            }
            Ret::Void
        }),
    );
}

/// The guest address of a host pointer inside guest memory.
fn guest_address(host: usize) -> Option<u32> {
    let base = mem().host(0) as usize;
    (host >= base && host - base < (1usize << 32)).then(|| (host - base) as u32)
}
