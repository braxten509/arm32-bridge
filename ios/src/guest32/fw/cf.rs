//! Core Foundation's C functions for 32-bit games: this layer's (CF types
//! are its Foundation objects), with CFIndex, CFNumber types and
//! pointer-sized results converted; also the framework string and number
//! constants the games bind as data.

use super::{host_fn, host_functions, object_to_guest, object_to_host, register_all, Bool, CStr, Handle, Long, Obj, Ptr, Word, U32};
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

pub fn register() {
    register_all(host_functions! {
        "CFRetain" => fn(object: Obj) -> Obj;
        "CFGetTypeID" => fn(object: Obj) -> Word;
        "CFRunLoopGetCurrent" => fn() -> Obj;
        "CFUUIDCreate" => fn(allocator: Ptr) -> Obj;
        "CFUUIDCreateString" => fn(allocator: Ptr, uuid: Obj) -> Obj;
        "CFUUIDCreateFromString" => fn(allocator: Ptr, text: Obj) -> Obj;
        "CFURLCreateStringByAddingPercentEscapes" => fn(allocator: Ptr, text: Obj, leave: Obj, escape: Obj, encoding: U32) -> Obj;
        "CFURLCreateStringByReplacingPercentEscapesUsingEncoding" => fn(allocator: Ptr, text: Obj, leave: Obj, encoding: U32) -> Obj;
        "CFURLCopyPathExtension" => fn(url: Obj) -> Obj;
        "CFStringGetCString" => fn(text: Obj, buffer: Ptr, size: Long, encoding: U32) -> Bool;
        "CFStringGetCStringPtr" => fn(text: Obj, encoding: U32) -> CStr;
        "CFStringCreateWithCharacters" => fn(allocator: Ptr, characters: Ptr, count: Long) -> Obj;
        "CFStringAppendCharacters" => fn(text: Obj, characters: Ptr, count: Long);
        "CFStringCompare" => fn(a: Obj, b: Obj, flags: Word) -> Long;
        "CFStringConvertNSStringEncodingToEncoding" => fn(encoding: Word) -> U32;
        "CFStringConvertEncodingToNSStringEncoding" => fn(encoding: U32) -> Word;
        "CFDictionaryGetValue" => fn(dictionary: Obj, key: Obj) -> Obj;
        "CFBitVectorCreate" => fn(allocator: Ptr, bytes: Ptr, count: Long) -> Obj;
        "CFBitVectorCreateCopy" => fn(allocator: Ptr, vector: Obj) -> Obj;
        "CFBitVectorCreateMutable" => fn(allocator: Ptr, capacity: Long) -> Obj;
        "CFBitVectorCreateMutableCopy" => fn(allocator: Ptr, capacity: Long, vector: Obj) -> Obj;
        "CFBitVectorGetCount" => fn(vector: Obj) -> Long;
        "CFBitVectorSetCount" => fn(vector: Obj, count: Long);
        "CFBitVectorGetBitAtIndex" => fn(vector: Obj, index: Long) -> U32;
        "CFBitVectorSetBitAtIndex" => fn(vector: Obj, index: Long, value: U32);
        "NSSearchPathForDirectoriesInDomains" => fn(directory: Word, domains: Word, expand: Bool) -> Obj;
        // SystemConfiguration: the layer reports no network and never calls the callback.
        "SCNetworkReachabilityCreateWithName" => fn(allocator: Ptr, name: Ptr) -> Handle;
        "SCNetworkReachabilityCreateWithAddress" => fn(allocator: Ptr, address: Ptr) -> Handle;
        "SCNetworkReachabilityGetFlags" => fn(target: Handle, flags: Ptr) -> Bool;
        "SCNetworkReachabilitySetCallback" => fn(target: Handle, callout: Word, context: Ptr) -> Bool;
        "SCNetworkReachabilityScheduleWithRunLoop" => fn(target: Handle, run_loop: Obj, mode: Obj) -> Bool;
        "SCNetworkReachabilityUnscheduleFromRunLoop" => fn(target: Handle, run_loop: Obj, mode: Obj) -> Bool;
        "SCNetworkReachabilitySetDispatchQueue" => fn(target: Handle, queue: Word) -> Bool;
    });
    // CFRelease(object): a data provider's release callback is guest code,
    // so it runs here, after the host let go of the last reference.
    super::super::function(
        "CFRelease",
        handler!(|uc, a| {
            let object = object_to_host(a.u32());
            if object != 0 {
                if let Some(release) = host_fn!("CFRelease": unsafe extern "C" fn(usize)) {
                    unsafe { release(object) };
                }
            }
            super::graphics::run_pending_releases(uc);
            Ret::Void
        })
        .unwrap(),
    );
    // CFNumberGetValue(number, type, out): the long-sized types are 4 bytes
    // here and CGFloat a float, 8 bytes in the host.
    super::super::function(
        "CFNumberGetValue",
        handler!(|uc, a| {
            let (number, kind, out) = (object_to_host(a.u32()), a.i32(), a.u32());
            const SINT32: isize = 3;
            const FLOAT32: isize = 5;
            let kind = match kind {
                10 | 14 | 15 => SINT32, // long, CFIndex, NSInteger
                16 => FLOAT32,          // CGFloat
                other => other as isize,
            };
            let Some(get) = host_fn!("CFNumberGetValue": unsafe extern "C" fn(usize, isize, *mut u8) -> bool) else {
                return super::no_host_function(uc, "CFNumberGetValue");
            };
            Ret::U32(unsafe { get(number, kind, mem().host_or_null(out)) } as u32)
        })
        .unwrap(),
    );
    // CFPropertyListCreateFromXMLData(allocator, data, options, &errorString).
    super::super::function(
        "CFPropertyListCreateFromXMLData",
        handler!(|uc, a| {
            let (_allocator, data, options, error) = (a.u32(), object_to_host(a.u32()), a.u32(), a.u32());
            type Create = unsafe extern "C" fn(usize, usize, usize, *mut usize) -> usize;
            let Some(create) = host_fn!("CFPropertyListCreateFromXMLData": Create) else {
                return super::no_host_function(uc, "CFPropertyListCreateFromXMLData");
            };
            let mut message = 0usize;
            let list = unsafe { create(0, data, options as usize, &mut message) };
            if error != 0 {
                mem().write_u32(error, object_to_guest(message));
            }
            Ret::U32(object_to_guest(list))
        })
        .unwrap(),
    );
    for name in WORD_CONSTANTS {
        if super::host_data(name).is_some() {
            super::word_constant(name);
        }
    }
    for name in OBJECT_CONSTANTS {
        if super::host_data(name).is_some() {
            super::object_constant(name);
        }
    }
}

/// Pointer-sized number constants.
const WORD_CONSTANTS: &[&str] = &["kCFAllocatorDefault", "UIBackgroundTaskInvalid"];

/// Framework object constants (strings, booleans) bound as data: each is a
/// guest variable holding the guest form of this layer's object.
const OBJECT_CONSTANTS: &[&str] = &[
    // Core Foundation
    "kCFRunLoopDefaultMode", "kCFRunLoopCommonModes", "kCFBooleanTrue", "kCFBooleanFalse", "kCFBundleIdentifierKey",
    "kCFBundleVersionKey", "kCFLocaleCurrencySymbol",
    // Foundation
    "NSDefaultRunLoopMode", "NSRunLoopCommonModes", "NSInternalInconsistencyException", "NSInvalidArgumentException",
    "NSLocalizedDescriptionKey", "NSLocalizedFailureReasonErrorKey", "NSUnderlyingErrorKey", "NSPOSIXErrorDomain",
    "NSURLErrorDomain", "NSLocaleLanguageCode", "NSLocaleCountryCode", "NSLocaleCurrencyCode", "NSLocaleCurrencySymbol",
    "NSGregorianCalendar", "NSCalendarIdentifierGregorian", "NSMetadataItemFSNameKey", "NSMetadataItemFSCreationDateKey",
    "NSMetadataItemURLKey", "NSMetadataQueryDidFinishGatheringNotification", "NSMetadataQueryUbiquitousDocumentsScope",
    "NSURLUbiquitousItemIsDownloadedKey", "NSFileSystemFreeSize", "NSFileSystemSize",
    // UIKit
    "UIApplicationDidBecomeActiveNotification", "UIApplicationWillResignActiveNotification",
    "UIApplicationWillTerminateNotification", "UIApplicationDidEnterBackgroundNotification",
    "UIApplicationWillEnterForegroundNotification", "UIApplicationDidFinishLaunchingNotification",
    "UIApplicationLaunchOptionsURLKey", "UIDeviceOrientationDidChangeNotification", "UIKeyboardWillShowNotification",
    "UIKeyboardDidShowNotification", "UIKeyboardWillHideNotification", "UIKeyboardDidHideNotification",
    "UIKeyboardWillChangeFrameNotification", "UIKeyboardFrameBeginUserInfoKey", "UIKeyboardFrameEndUserInfoKey",
    "UIKeyboardAnimationDurationUserInfoKey", "UIKeyboardAnimationCurveUserInfoKey",
    // AVFoundation, OpenGL ES (EAGL), Accounts
    "AVAudioSessionCategoryAmbient", "AVAudioSessionCategorySoloAmbient", "AVAudioSessionCategoryPlayback",
    "AVAudioSessionCategoryRecord", "AVAudioSessionCategoryPlayAndRecord", "kEAGLDrawablePropertyColorFormat",
    "kEAGLDrawablePropertyRetainedBacking", "kEAGLColorFormatRGBA8", "kEAGLColorFormatRGB565",
    "ACAccountTypeIdentifierTwitter", "ACAccountTypeIdentifierFacebook",
];
