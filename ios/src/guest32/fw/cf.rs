//! Core Foundation's C functions for 32-bit games: this layer's (CF types
//! are its Foundation objects), with CFIndex, CFNumber types and
//! pointer-sized results converted; also the framework string and number
//! constants the games bind as data.

use super::{host_fn, host_functions, object_to_guest, object_to_host, register_all, Bool, CStr, Handle, I32, Long, Obj, Ptr, Word, F64, U32};
use crate::fw::cfbridge::CFRange;
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

/// The host form of a CFNumberType: the long-sized types are 4 bytes here
/// and CGFloat a float, 8 bytes in the host.
fn host_number_type(kind: i32) -> isize {
    const SINT32: isize = 3;
    const FLOAT32: isize = 5;
    match kind {
        10 | 14 | 15 => SINT32, // long, CFIndex, NSInteger
        16 => FLOAT32,          // CGFloat
        other => other as isize,
    }
}

/// Writes host objects as guest words (`out` NULL: nothing).
fn write_objects(out: u32, objects: &[usize]) {
    if out != 0 {
        for (i, object) in objects.iter().enumerate() {
            mem().write_u32(out + i as u32 * 4, object_to_guest(*object));
        }
    }
}

pub fn register() {
    register_all(host_functions! {
        "CFRetain" => fn(object: Obj) -> Obj;
        "CFEqual" => fn(a: Obj, b: Obj) -> Bool;
        "CFHash" => fn(object: Obj) -> Word;
        "CFMakeCollectable" => fn(object: Obj) -> Obj;
        "CFAbsoluteTimeGetCurrent" => fn() -> F64;
        "CFLocaleGetSystem" => fn() -> Obj;
        "CFArrayGetCount" => fn(array: Obj) -> Long;
        "CFArrayGetValueAtIndex" => fn(array: Obj, index: Long) -> Obj;
        "CFDictionaryGetCount" => fn(dictionary: Obj) -> Long;
        // Data bytes live in guest memory (fw::data), so their pointers cross as they are.
        "CFDataCreate" => fn(allocator: Ptr, bytes: Ptr, length: Long) -> Obj;
        "CFDataCreateMutable" => fn(allocator: Ptr, capacity: Long) -> Obj;
        "CFDataCreateWithBytesNoCopy" => fn(allocator: Ptr, bytes: Ptr, length: Long, deallocator: Obj) -> Obj;
        "CFDataGetBytePtr" => fn(data: Obj) -> Ptr;
        "CFDataGetMutableBytePtr" => fn(data: Obj) -> Ptr;
        "CFDataGetLength" => fn(data: Obj) -> Long;
        "CFDataSetLength" => fn(data: Obj, length: Long);
        "CFStringGetLength" => fn(text: Obj) -> Long;
        "CFStringCreateMutable" => fn(allocator: Ptr, max_length: Long) -> Obj;
        "CFStringLowercase" => fn(text: Obj, locale: Obj);
        "CFStringConvertIANACharSetNameToEncoding" => fn(name: Obj) -> U32;
        "CFStringGetFastestEncoding" => fn(text: Obj) -> U32;
        "CFStringGetMaximumSizeForEncoding" => fn(length: Long, encoding: U32) -> Long;
        "CFStringCreateWithBytes" => fn(allocator: Ptr, bytes: Ptr, length: Long, encoding: U32, external: Bool) -> Obj;
        "CFStringCreateWithBytesNoCopy" => fn(allocator: Ptr, bytes: Ptr, length: Long, encoding: U32, external: Bool, deallocator: Obj) -> Obj;
        "NSTemporaryDirectory" => fn() -> Obj;
        "CFGetTypeID" => fn(object: Obj) -> Word;
        "CFRunLoopGetCurrent" => fn() -> Obj;
        "CFRunLoopRunInMode" => fn(mode: Obj, seconds: F64, return_after: Bool) -> I32;
        "CFBundleGetMainBundle" => fn() -> Obj;
        "CFBundleCopyBundleURL" => fn(bundle: Obj) -> Obj;
        "CFURLCopyFileSystemPath" => fn(url: Obj, style: I32) -> Obj;
        "NSHomeDirectory" => fn() -> Obj;
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
    // CFRelease(object): a data provider's release callback is guest code
    // the host runs from inside the release, before this returns.
    super::super::function(
        "CFRelease",
        handler!(|_uc, a| {
            let object = object_to_host(a.u32());
            if object != 0 {
                if let Some(release) = host_fn!("CFRelease": unsafe extern "C" fn(usize)) {
                    unsafe { release(object) };
                }
            }
            Ret::Void
        })
        .unwrap(),
    );
    // CFNumberGetValue(number, type, out): the long-sized types are 4 bytes
    // here and CGFloat a float, 8 bytes in the host.
    super::super::function(
        "CFNumberGetValue",
        handler!(|uc, a| {
            let (number, kind, out) = (object_to_host(a.u32()), host_number_type(a.i32()), a.u32());
            let Some(get) = host_fn!("CFNumberGetValue": unsafe extern "C" fn(usize, isize, *mut u8) -> bool) else {
                return super::no_host_function(uc, "CFNumberGetValue");
            };
            Ret::U32(unsafe { get(number, kind, mem().host_or_null(out)) } as u32)
        })
        .unwrap(),
    );
    // CFNumberCreate(allocator, type, &value): the value in its 32-bit size.
    super::super::function(
        "CFNumberCreate",
        handler!(|uc, a| {
            let (_allocator, kind, value) = (a.u32(), host_number_type(a.i32()), a.u32());
            let Some(create) = host_fn!("CFNumberCreate": unsafe extern "C" fn(usize, isize, *const u8) -> usize) else {
                return super::no_host_function(uc, "CFNumberCreate");
            };
            Ret::U32(object_to_guest(unsafe { create(0, kind, mem().host_or_null(value)) }))
        })
        .unwrap(),
    );
    // CFArrayGetValues(array, CFRange, values): 4-byte object words.
    super::super::function(
        "CFArrayGetValues",
        handler!(|uc, a| {
            let (array, location, length, out) = (object_to_host(a.u32()), a.i32() as isize, a.i32() as isize, a.u32());
            let Some(get) = host_fn!("CFArrayGetValues": unsafe extern "C" fn(usize, CFRange, *mut usize)) else {
                return super::no_host_function(uc, "CFArrayGetValues");
            };
            let mut values = vec![0usize; length.max(0) as usize];
            unsafe { get(array, CFRange { location, length }, values.as_mut_ptr()) };
            write_objects(out, &values);
            Ret::Void
        })
        .unwrap(),
    );
    // CFDictionaryGetKeysAndValues(dictionary, keys, values): either may be NULL.
    super::super::function(
        "CFDictionaryGetKeysAndValues",
        handler!(|uc, a| {
            let (dictionary, keys_out, values_out) = (object_to_host(a.u32()), a.u32(), a.u32());
            let (Some(count), Some(get)) = (
                host_fn!("CFDictionaryGetCount": unsafe extern "C" fn(usize) -> isize),
                host_fn!("CFDictionaryGetKeysAndValues": unsafe extern "C" fn(usize, *mut usize, *mut usize)),
            ) else {
                return super::no_host_function(uc, "CFDictionaryGetKeysAndValues");
            };
            let count = unsafe { count(dictionary) }.max(0) as usize;
            let (mut keys, mut values) = (vec![0usize; count], vec![0usize; count]);
            unsafe { get(dictionary, keys.as_mut_ptr(), values.as_mut_ptr()) };
            write_objects(keys_out, &keys);
            write_objects(values_out, &values);
            Ret::Void
        })
        .unwrap(),
    );
    // CFStringGetBytes(string, CFRange, encoding, loss byte, external, buffer, room, &used):
    // the bytes are written straight into the game's buffer; *used is a 4-byte CFIndex.
    super::super::function(
        "CFStringGetBytes",
        handler!(|uc, a| {
            let (text, location, length, encoding) = (object_to_host(a.u32()), a.i32() as isize, a.i32() as isize, a.u32());
            let (loss, external, buffer, room, used) = (a.u32() as u8, a.u32() & 0xff != 0, a.u32(), a.i32() as isize, a.u32());
            type GetBytes = unsafe extern "C" fn(usize, CFRange, u32, u8, bool, *mut u8, isize, *mut isize) -> isize;
            let Some(get) = host_fn!("CFStringGetBytes": GetBytes) else {
                return super::no_host_function(uc, "CFStringGetBytes");
            };
            let mut written = 0isize;
            let range = CFRange { location, length };
            let converted = unsafe { get(text, range, encoding, loss, external, mem().host_or_null(buffer), room, &mut written) };
            if used != 0 {
                mem().write_u32(used, written as u32);
            }
            Ret::U32(converted as u32)
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
    for name in OBJECT_CONSTANTS.iter().chain(crate::fw::animation::CONSTANT_NAMES) {
        if super::host_data(name).is_some() {
            super::object_constant(name);
        }
    }
    for name in ["UIWindowLevelNormal", "UIWindowLevelStatusBar", "UIWindowLevelAlert"] {
        super::float_constant(name);
    }
    // iOS 6.1's CoreFoundation version: the era these armv7 games were built
    // for (Apple's constant is a double, four-byte aligned on armv7).
    super::super::data("kCFCoreFoundationVersionNumber", || {
        let cell = crate::guest32::alloc_permanent(8, 4);
        mem().write_u64(cell, 793.0f64.to_bits());
        cell
    });
}

/// Pointer-sized number constants.
const WORD_CONSTANTS: &[&str] = &["kCFAllocatorDefault", "UIBackgroundTaskInvalid"];

/// Framework object constants (strings, booleans) bound as data: each is a
/// guest variable holding the guest form of this layer's object.
const OBJECT_CONSTANTS: &[&str] = &[
    // Core Foundation
    "kCFRunLoopDefaultMode", "kCFRunLoopCommonModes", "kCFBooleanTrue", "kCFBooleanFalse", "kCFBundleIdentifierKey",
    "kCFBundleVersionKey", "kCFLocaleCurrencySymbol", "kCFNull", "kCFNumberNaN", "kCFNumberNegativeInfinity",
    "kCFNumberPositiveInfinity", "kCFAllocatorNull", "kCFAllocatorMalloc", "kCFAllocatorSystemDefault",
    // Security (the keychain)
    "kSecClass", "kSecClassGenericPassword", "kSecClassInternetPassword", "kSecClassKey", "kSecAttrService",
    "kSecAttrAccount", "kSecAttrAccessGroup", "kSecAttrAccessible", "kSecAttrAccessibleWhenUnlocked",
    "kSecAttrAccessibleAfterFirstUnlock", "kSecAttrAccessibleAlways", "kSecAttrGeneric", "kSecAttrLabel",
    "kSecAttrApplicationTag", "kSecAttrApplicationLabel", "kSecAttrKeyClass", "kSecAttrKeyClassPublic",
    "kSecAttrKeyClassPrivate", "kSecAttrKeyType", "kSecAttrKeyTypeRSA", "kSecAttrKeySizeInBits", "kSecValueData",
    "kSecValuePersistentRef", "kSecReturnData", "kSecReturnAttributes", "kSecReturnRef", "kSecReturnPersistentRef", "kSecMatchLimit",
    "kSecMatchLimitOne", "kSecMatchLimitAll",
    // StoreKit, MediaPlayer, Core Animation
    "SKErrorDomain", "MPMovieDurationAvailableNotification", "MPMoviePlayerContentPreloadDidFinishNotification",
    "MPMoviePlayerLoadStateDidChangeNotification", "MPMoviePlayerPlaybackDidFinishNotification",
    "MPMoviePlayerPlaybackDidFinishReasonUserInfoKey", "MPMoviePlayerPlaybackStateDidChangeNotification",
    "MPMoviePlayerNowPlayingMovieDidChangeNotification", "MPMediaLibraryDidChangeNotification",
    "MPMusicPlayerControllerNowPlayingItemDidChangeNotification",
    "MPMusicPlayerControllerPlaybackStateDidChangeNotification", "MPMediaItemPropertyPersistentID",
    // (Core Animation's are fw::animation::CONSTANT_NAMES, below.)
    // Foundation
    "NSDefaultRunLoopMode", "NSRunLoopCommonModes", "NSInternalInconsistencyException", "NSInvalidArgumentException",
    "NSLocalizedDescriptionKey", "NSLocalizedFailureReasonErrorKey", "NSUnderlyingErrorKey", "NSPOSIXErrorDomain",
    "NSURLErrorDomain", "NSLocaleLanguageCode", "NSLocaleCountryCode", "NSLocaleCurrencyCode", "NSLocaleCurrencySymbol",
    "NSGregorianCalendar", "NSCalendarIdentifierGregorian", "NSMetadataItemFSNameKey", "NSMetadataItemFSCreationDateKey",
    "NSMetadataItemURLKey", "NSMetadataQueryDidFinishGatheringNotification", "NSMetadataQueryUbiquitousDocumentsScope",
    "NSURLUbiquitousItemIsDownloadedKey", "NSFileSystemFreeSize", "NSFileSystemSize", "NSFileModificationDate",
    // AddressBook
    "kABWorkLabel",
    // UIKit
    "UIApplicationDidBecomeActiveNotification", "UIApplicationWillResignActiveNotification",
    "UIApplicationWillTerminateNotification", "UIApplicationDidEnterBackgroundNotification",
    "UIApplicationWillEnterForegroundNotification", "UIApplicationDidFinishLaunchingNotification",
    "UIApplicationLaunchOptionsURLKey", "UIApplicationDidReceiveMemoryWarningNotification",
    "UIApplicationWillChangeStatusBarOrientationNotification", "UIApplicationDidChangeStatusBarOrientationNotification",
    "UIApplicationStatusBarOrientationUserInfoKey", "UITrackingRunLoopMode",
    "UIApplicationDidChangeStatusBarFrameNotification", "UILocalNotificationDefaultSoundName",
    "UIDeviceOrientationDidChangeNotification", "UIKeyboardWillShowNotification",
    "UIKeyboardDidShowNotification", "UIKeyboardWillHideNotification", "UIKeyboardDidHideNotification",
    "UIKeyboardWillChangeFrameNotification", "UIKeyboardFrameBeginUserInfoKey", "UIKeyboardFrameEndUserInfoKey",
    "UIKeyboardAnimationDurationUserInfoKey", "UIKeyboardAnimationCurveUserInfoKey",
    // AVFoundation, OpenGL ES (EAGL), Accounts
    "AVAudioSessionCategoryAmbient", "AVAudioSessionCategorySoloAmbient", "AVAudioSessionCategoryPlayback",
    "AVAudioSessionCategoryRecord", "AVAudioSessionCategoryPlayAndRecord", "AVAudioSessionInterruptionNotification",
    "AVAudioSessionInterruptionTypeKey", "AVAudioSessionInterruptionOptionKey", "kEAGLDrawablePropertyColorFormat",
    "kEAGLDrawablePropertyRetainedBacking", "kEAGLColorFormatRGBA8", "kEAGLColorFormatRGB565",
    "ACAccountTypeIdentifierTwitter", "ACAccountTypeIdentifierFacebook",
];
