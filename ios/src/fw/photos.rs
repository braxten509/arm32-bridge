//! AssetsLibrary: the photo library. There is none here, so it answers as
//! an iPhone does when the user has denied access: the status is "denied",
//! and every look-up or save reports ALAssetsLibraryAccessUserDeniedError
//! through the game's failure block, later, on the main thread.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{misc, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::sync::OnceLock;

const ERROR_DOMAIN: &str = "ALAssetsLibraryErrorDomain";
const ACCESS_USER_DENIED: isize = -3311;
/// ALAuthorizationStatusDenied.
const DENIED: isize = 2;

fn denied() -> Id {
    misc::error(ERROR_DOMAIN, ACCESS_USER_DENIED, "User denied access")
}

/// Calls `block(error)` (or `block(nil, error)` when `with_url`) later on the
/// main thread, as AssetsLibrary does.
fn fail_later(block: usize, with_url: bool) {
    if block == 0 {
        return;
    }
    let copy: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(crate::libsystem::mach::block_copy as usize) };
    let block = copy(block);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let invoke = unsafe { *((block + 16) as *const usize) };
        let error = denied();
        if with_url {
            let f: extern "C" fn(usize, Id, Id) = unsafe { std::mem::transmute(invoke) };
            f(block, 0, error);
        } else {
            let f: extern "C" fn(usize, Id) = unsafe { std::mem::transmute(invoke) };
            f(block, error);
        }
        // The copy goes once the block has run.
        crate::libsystem::mach::block_release(block as *mut _);
    })));
}

extern "C" fn authorization_status(_class: Class, _: Sel) -> isize {
    DENIED
}

extern "C" fn class_nothing(_class: Class, _: Sel) {}

extern "C" fn enumerate_groups(_this: Id, _: Sel, _types: u32, _each: usize, failure: usize) {
    crate::log!("ALAssetsLibrary: the photo library was asked for; access denied (no photo library here)");
    fail_later(failure, false);
}

extern "C" fn asset_for_url(_this: Id, _: Sel, _url: Id, _result: usize, failure: usize) {
    fail_later(failure, false);
}

extern "C" fn group_for_url(_this: Id, _: Sel, _url: Id, _result: usize, failure: usize) {
    fail_later(failure, false);
}

extern "C" fn add_group(_this: Id, _: Sel, _name: Id, _result: usize, failure: usize) {
    fail_later(failure, false);
}

/// writeImageToSavedPhotosAlbum:orientation:completionBlock: and the other
/// saves: the completion block gets no URL and the "denied" error.
extern "C" fn write_image(_this: Id, _: Sel, _image: Id, _orientation: isize, completion: usize) {
    crate::log!("ALAssetsLibrary: a picture was to be saved to the photo library; access denied");
    fail_later(completion, true);
}

extern "C" fn write_with_metadata(_this: Id, _: Sel, _data: Id, _metadata: Id, completion: usize) {
    crate::log!("ALAssetsLibrary: a picture was to be saved to the photo library; access denied");
    fail_later(completion, true);
}

extern "C" fn write_video(_this: Id, _: Sel, _url: Id, completion: usize) {
    crate::log!("ALAssetsLibrary: a video was to be saved to the photo library; access denied");
    fail_later(completion, true);
}

extern "C" fn video_compatible(_this: Id, _: Sel, _url: Id) -> bool {
    false
}

/// ALAssetsFilter's three filters (one object each, as on iOS).
fn filter(which: usize) -> Id {
    static FILTERS: OnceLock<[Id; 3]> = OnceLock::new();
    FILTERS.get_or_init(|| {
        let class = runtime::class_named("ALAssetsFilter");
        [runtime::alloc_instance(class), runtime::alloc_instance(class), runtime::alloc_instance(class)]
    })[which]
}

extern "C" fn all_assets(_class: Class, _: Sel) -> Id {
    filter(0)
}

extern "C" fn all_photos(_class: Class, _: Sel) -> Id {
    filter(1)
}

extern "C" fn all_videos(_class: Class, _: Sel) -> Id {
    filter(2)
}

pub fn register(r: &mut Registry) {
    let library = define_class("ALAssetsLibrary", "NSObject", 8);
    class_methods(library, &[("authorizationStatus", authorization_status as usize), ("disableSharedPhotoStreamsSupport", class_nothing as usize)]);
    methods(
        library,
        &[
            ("enumerateGroupsWithTypes:usingBlock:failureBlock:", enumerate_groups as usize),
            ("assetForURL:resultBlock:failureBlock:", asset_for_url as usize),
            ("groupForURL:resultBlock:failureBlock:", group_for_url as usize),
            ("addAssetsGroupAlbumWithName:resultBlock:failureBlock:", add_group as usize),
            ("writeImageToSavedPhotosAlbum:orientation:completionBlock:", write_image as usize),
            ("writeImageToSavedPhotosAlbum:metadata:completionBlock:", write_with_metadata as usize),
            ("writeImageDataToSavedPhotosAlbum:metadata:completionBlock:", write_with_metadata as usize),
            ("writeVideoAtPathToSavedPhotosAlbum:completionBlock:", write_video as usize),
            ("videoAtPathIsCompatibleWithSavedPhotosAlbum:", video_compatible as usize),
        ],
    );
    let filters = define_class("ALAssetsFilter", "NSObject", 8);
    class_methods(filters, &[("allAssets", all_assets as usize), ("allPhotos", all_photos as usize), ("allVideos", all_videos as usize)]);
    define_class("ALAsset", "NSObject", 8);
    define_class("ALAssetsGroup", "NSObject", 8);
    define_class("ALAssetRepresentation", "NSObject", 8);

    // Apple's own texts for these names.
    for name in [
        "ALAssetPropertyType",
        "ALAssetPropertyLocation",
        "ALAssetPropertyDuration",
        "ALAssetPropertyOrientation",
        "ALAssetPropertyDate",
        "ALAssetPropertyRepresentations",
        "ALAssetPropertyURLs",
        "ALAssetPropertyAssetURL",
        "ALAssetTypePhoto",
        "ALAssetTypeVideo",
        "ALAssetTypeUnknown",
        "ALAssetsGroupPropertyName",
        "ALAssetsGroupPropertyType",
        "ALAssetsGroupPropertyPersistentID",
        "ALAssetsGroupPropertyURL",
        "ALAssetsLibraryChangedNotification",
        "ALAssetLibraryUpdatedAssetsKey",
        "ALAssetsLibraryErrorDomain",
    ] {
        r.data(name, leak(string::make(name)));
    }
}
