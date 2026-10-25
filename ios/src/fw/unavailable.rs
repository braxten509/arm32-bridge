//! Services this phone doesn't offer to iPhone games, answering exactly as
//! an iPhone does when they are unavailable or the user cancels: ads (iAd)
//! never load, mail and text messages can't be sent (MessageUI), location
//! services are turned off (CoreLocation), and web pages (SafariServices)
//! close straight away.

use crate::fw::controls::{later, not_drawn, tell};
use crate::fw::geometry::{Rect, Size};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set, screen_points, view_state};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};

fn get_id(this: Id, key: &'static str) -> Id {
    property_get(this, key).unwrap_or(0) as Id
}

extern "C" fn delegate(this: Id, _: Sel) -> Id {
    get_id(this, "delegate")
}
extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}
extern "C" fn class_no(_: Class, _: Sel) -> bool {
    false
}
extern "C" fn nothing(_this: Id, _: Sel) {}
extern "C" fn nil(_this: Id, _: Sel) -> Id {
    0
}
extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}
extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}
extern "C" fn ignore_int(_this: Id, _: Sel, _value: isize) {}
extern "C" fn ignore_double(_this: Id, _: Sel, _value: f64) {}
extern "C" fn ignore_id_id_id(_this: Id, _: Sel, _a: Id, _b: Id, _c: Id) {}
extern "C" fn no_with_id(_this: Id, _: Sel, _value: Id) -> bool {
    false
}

// ----- iAd: no ads -----

/// ADErrorInventoryUnavailable: what iAd reports when it has no ad to show.
const AD_INVENTORY_UNAVAILABLE: isize = 3;

fn no_ad_error() -> Id {
    crate::fw::misc::error("ADErrorDomain", AD_INVENTORY_UNAVAILABLE, "Ad inventory unavailable")
}

/// The banner size iAd uses on this screen: the screen's width, 32 points
/// tall in landscape (50 in portrait); a medium rectangle is 300x250.
fn banner_size(kind: isize) -> Size {
    if kind == 1 {
        return Size { width: 300.0, height: 250.0 };
    }
    let (w, h, _) = screen_points();
    Size { width: w, height: if w > h { 32.0 } else { 50.0 } }
}

fn banner_setup(this: Id, kind: isize) -> Id {
    property_set(this, "adType", kind as u64);
    property_set(this, "notDrawn", 1);
    view_state(this).frame.size = banner_size(kind);
    this
}

fn super_init_frame(this: Id, frame: Rect) -> Id {
    let s = runtime::sel("initWithFrame:");
    let view = runtime::class_named("UIView"); // before taking the lock: it locks too
    let imp = lock().lookup(view, s).expect("UIView initWithFrame:");
    let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
    f(this, s, frame)
}

extern "C" fn banner_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, frame);
    banner_setup(this, 0)
}

extern "C" fn banner_init_with_type(this: Id, _: Sel, kind: isize) -> Id {
    super_init_frame(this, Rect::default());
    banner_setup(this, kind)
}

extern "C" fn banner_type(this: Id, _: Sel) -> isize {
    property_get(this, "adType").unwrap_or(0) as isize
}

/// Once it has a delegate, the banner reports it couldn't get an ad.
extern "C" fn banner_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
    if delegate == 0 {
        return;
    }
    later(move || {
        if get_id(this, "delegate") == delegate {
            crate::log!("ad banner: no ads here, telling the game none was received");
            tell(delegate, "bannerView:didFailToReceiveAdWithError:", &[this, no_ad_error()]);
        }
    });
}

extern "C" fn banner_size_for_identifier(_: Class, _: Sel, _identifier: Id) -> Size {
    banner_size(0)
}

extern "C" fn interstitial_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
    if delegate == 0 {
        return;
    }
    later(move || {
        if get_id(this, "delegate") == delegate {
            crate::log!("full-screen ad: no ads here, telling the game it failed");
            tell(delegate, "interstitialAd:didFailWithError:", &[this, no_ad_error()]);
        }
    });
}

// ----- MessageUI: no mail or text messages -----

/// Mail and message screens can't send anything here (canSendMail and
/// canSendText say NO); one shown anyway answers as if the user cancelled.
extern "C" fn mail_answer_shown(this: Id, _: Sel) {
    crate::log!("mail screen shown: no mail here, telling the game the user cancelled");
    // MFMailComposeResultCancelled, no error.
    tell(get_id(this, "mailComposeDelegate"), "mailComposeController:didFinishWithResult:error:", &[this, 0, 0]);
}

extern "C" fn message_answer_shown(this: Id, _: Sel) {
    crate::log!("text message screen shown: no messages here, telling the game the user cancelled");
    // MessageComposeResultCancelled.
    tell(get_id(this, "messageComposeDelegate"), "messageComposeViewController:didFinishWithResult:", &[this, 0]);
}

extern "C" fn mail_delegate(this: Id, _: Sel) -> Id {
    get_id(this, "mailComposeDelegate")
}
extern "C" fn mail_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "mailComposeDelegate", delegate as u64);
}
extern "C" fn message_delegate(this: Id, _: Sel) -> Id {
    get_id(this, "messageComposeDelegate")
}
extern "C" fn message_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "messageComposeDelegate", delegate as u64);
}
extern "C" fn ignore_body_html(_this: Id, _: Sel, _body: Id, _html: u8) {}

// ----- CoreLocation: location services are off -----

/// kCLAuthorizationStatusDenied: also what iOS says when location services
/// are turned off for the whole phone.
const AUTHORIZATION_DENIED: isize = 2;
/// kCLErrorDenied.
const LOCATION_DENIED: isize = 1;

fn denied_error() -> Id {
    crate::fw::misc::error("kCLErrorDomain", LOCATION_DENIED, "The operation couldn’t be completed. (kCLErrorDomain error 1.)")
}

extern "C" fn class_authorization(_: Class, _: Sel) -> isize {
    AUTHORIZATION_DENIED
}

extern "C" fn class_monitoring_available(_: Class, _: Sel, _region_class: Class) -> bool {
    false
}

/// Core Location calls the delegate on the run loop of the thread that
/// made the manager; init remembers it.
extern "C" fn location_init(this: Id, _: Sel) -> Id {
    let run_loop: &'static crate::fw::runloop::Loop = crate::fw::runloop::this_threads_loop();
    property_set(this, "runLoop", run_loop as *const _ as u64);
    this
}

/// How long an answer waits for a thread that has never run its run loop
/// before it goes to the main run loop instead.
const UNRUN_LOOP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

fn later_for_manager(manager: Id, work: impl FnOnce() + Send + 'static) {
    use crate::fw::runloop::{post_to, Loop, Work};
    let Some(run_loop) = property_get(manager, "runLoop") else {
        later(work);
        return;
    };
    let run_loop: &'static Loop = unsafe { &*(run_loop as *const Loop) };
    if run_loop.runs() {
        post_to(run_loop, Work::Native(Box::new(work)));
        return;
    }
    // The thread that made the manager hasn't run its loop (yet). Post it
    // there, and if it still hasn't been taken a moment later, answer on
    // the main run loop rather than lose it.
    type Pending = std::sync::Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>>;
    let pending: Pending = std::sync::Arc::new(std::sync::Mutex::new(Some(Box::new(work))));
    let on_thread = pending.clone();
    post_to(
        run_loop,
        Work::Native(Box::new(move || {
            let work = on_thread.lock().unwrap().take();
            if let Some(work) = work {
                work();
            }
        })),
    );
    std::thread::spawn(move || {
        std::thread::sleep(UNRUN_LOOP_WAIT);
        later(move || {
            let work = pending.lock().unwrap().take();
            if let Some(work) = work {
                crate::log!("location: the thread that made the location manager never ran its run loop; answering on the main run loop");
                work();
            }
        });
    });
}

/// A new delegate hears the (denied) authorization, as on iOS.
extern "C" fn location_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
    if delegate == 0 {
        return;
    }
    later_for_manager(this, move || {
        if get_id(this, "delegate") == delegate {
            tell(delegate, "locationManager:didChangeAuthorizationStatus:", &[this, AUTHORIZATION_DENIED as usize]);
        }
    });
}

/// Starting updates fails with kCLErrorDenied.
extern "C" fn location_start(this: Id, _: Sel) {
    crate::log!("location asked for: location services are off here, telling the game it was denied");
    later_for_manager(this, move || {
        tell(get_id(this, "delegate"), "locationManager:didFailWithError:", &[this, denied_error()]);
    });
}

macro_rules! double_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> f64 {
            property_get(this, $key).map(f64::from_bits).unwrap_or($default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: f64) {
            property_set(this, $key, value.to_bits());
        }
    };
}
double_property!(location_accuracy, location_set_accuracy, "desiredAccuracy", -1.0);
double_property!(location_distance, location_set_distance, "distanceFilter", -1.0);
double_property!(location_heading_filter, location_set_heading_filter, "headingFilter", 1.0);

extern "C" fn location_purpose(this: Id, _: Sel) -> Id {
    get_id(this, "purpose")
}
extern "C" fn location_set_purpose(this: Id, _: Sel, purpose: Id) {
    property_set(this, "purpose", purpose as u64);
}

// ----- SafariServices: web pages can't be shown -----

extern "C" fn safari_init(this: Id, _: Sel, url: Id) -> Id {
    property_set(this, "URL", url as u64);
    this
}

extern "C" fn safari_init_reader(this: Id, s: Sel, url: Id, _reader: u8) -> Id {
    safari_init(this, s, url)
}

extern "C" fn safari_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
}

/// Shown, the page fails its first load and the screen closes, as when the
/// user taps Done on a page that couldn't load.
extern "C" fn safari_answer_shown(this: Id, _: Sel) {
    let address = crate::fw::string::describe(runtime::send(get_id(this, "URL"), "absoluteString", &[]));
    not_drawn(&format!("web page {address}"));
    let delegate = get_id(this, "delegate");
    tell(delegate, "safariViewController:didCompleteInitialLoad:", &[this, 0]);
    tell(delegate, "safariViewControllerDidFinish:", &[this]);
}

// ----- Accounts: no accounts are set up on this phone -----

/// ACErrorAccountNotFound, what iOS answers when no account of the type exists.
const ACCOUNT_NOT_FOUND: isize = 6;

extern "C" fn account_type_for_identifier(_this: Id, _: Sel, identifier: Id) -> Id {
    let kind = runtime::alloc_instance(runtime::class_named("ACAccountType"));
    property_set(kind, "identifier", crate::fw::string::make(&crate::fw::string::get(identifier)) as u64);
    kind
}

extern "C" fn no_accounts(_this: Id, _: Sel, _kind: Id) -> Id {
    crate::fw::collections::array_from(&[])
}

extern "C" fn all_accounts(_this: Id, _: Sel) -> Id {
    crate::fw::collections::array_from(&[])
}

extern "C" fn nil_with_id(_this: Id, _: Sel, _value: Id) -> Id {
    0
}

/// The completion block hears (NO, ACErrorAccountNotFound) on another
/// queue, as iOS answers it.
fn answer_no_access(handler: usize) {
    if handler == 0 {
        return;
    }
    crate::log!("accounts: access asked for; no accounts here, answering 'not found'");
    let handler = crate::libsystem::mach::block_copy(handler as *mut _) as usize;
    let call = crate::fw::media::make_callback(handler, |handler| {
        let error = crate::fw::misc::error("com.apple.accounts", ACCOUNT_NOT_FOUND, "The operation couldn’t be completed. (com.apple.accounts error 6.)");
        let header = handler as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, bool, Id) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(handler, false, error);
    });
    crate::libsystem::mach::dispatch_async(crate::libsystem::mach::dispatch_get_global_queue(0, 0), call);
}

extern "C" fn request_access_with_options(_this: Id, _: Sel, _kind: Id, _options: Id, handler: usize) {
    answer_no_access(handler);
}

extern "C" fn request_access(_this: Id, _: Sel, _kind: Id, handler: usize) {
    answer_no_access(handler);
}

extern "C" fn get_identifier(this: Id, _: Sel) -> Id {
    get_id(this, "identifier")
}

extern "C" fn account_init_with_type(this: Id, _: Sel, kind: Id) -> Id {
    property_set(this, "accountType", kind as u64);
    this
}

extern "C" fn account_type(this: Id, _: Sel) -> Id {
    get_id(this, "accountType")
}

extern "C" fn account_credential(this: Id, _: Sel) -> Id {
    get_id(this, "credential")
}

extern "C" fn account_set_credential(this: Id, _: Sel, credential: Id) {
    property_set(this, "credential", credential as u64);
}

extern "C" fn account_username(this: Id, _: Sel) -> Id {
    get_id(this, "username")
}

extern "C" fn account_set_username(this: Id, _: Sel, name: Id) {
    property_set(this, "username", name as u64);
}

// ----- AdSupport: ad tracking is limited -----

/// With "Limit Ad Tracking" on, iOS gives the all-zero identifier.
extern "C" fn advertising_identifier(_this: Id, _: Sel) -> Id {
    crate::fw::misc::uuid_from_string("00000000-0000-0000-0000-000000000000")
}

extern "C" fn shared_identifier_manager(_: Class, _: Sel) -> Id {
    static MANAGER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MANAGER.get_or_init(|| runtime::alloc_instance(runtime::class_named("ASIdentifierManager")))
}

// ----- Twitter: no Twitter account -----

extern "C" fn yes_with_id(_this: Id, _: Sel, _value: Id) -> bool {
    true
}

extern "C" fn yes(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn tweet_completion(this: Id, _: Sel) -> usize {
    property_get(this, "completionHandler").unwrap_or(0) as usize
}

extern "C" fn tweet_set_completion(this: Id, _: Sel, handler: usize) {
    let handler = if handler == 0 { 0 } else { crate::libsystem::mach::block_copy(handler as *mut _) as usize };
    property_set(this, "completionHandler", handler as u64);
}

/// Shown, the sheet closes as cancelled (iOS says there are no Twitter
/// accounts); with no handler set it closes itself, as iOS's default does.
extern "C" fn tweet_answer_shown(this: Id, _: Sel) {
    crate::log!("tweet sheet shown: no Twitter account here, telling the game it was cancelled");
    match property_get(this, "completionHandler").unwrap_or(0) as usize {
        // TWTweetComposeViewControllerResultCancelled.
        0 => {
            tell(this, "dismissViewControllerAnimated:completion:", &[1, 0]);
        }
        handler => crate::fw::controls::call_block_later(handler, [0, 0, 0]),
    }
}

// ----- CLLocation: a place, made by the game -----

#[repr(C)]
#[derive(Clone, Copy)]
struct Coordinate {
    latitude: f64,
    longitude: f64,
}

fn location_value(this: Id, key: &'static str, default: f64) -> f64 {
    property_get(this, key).map(f64::from_bits).unwrap_or(default)
}

extern "C" fn place_init_full(this: Id, _: Sel, at: Coordinate, altitude: f64, horizontal: f64, vertical: f64, timestamp: Id) -> Id {
    for (key, value) in [("latitude", at.latitude), ("longitude", at.longitude), ("altitude", altitude), ("horizontalAccuracy", horizontal), ("verticalAccuracy", vertical)] {
        property_set(this, key, value.to_bits());
    }
    let timestamp = if timestamp == 0 { crate::fw::misc::date(crate::fw::misc::now_since_2001()) } else { timestamp };
    property_set(this, "timestamp", timestamp as u64);
    this
}

/// initWithLatitude:longitude:: no altitude (vertical accuracy -1), now.
extern "C" fn place_init(this: Id, s: Sel, latitude: f64, longitude: f64) -> Id {
    place_init_full(this, s, Coordinate { latitude, longitude }, 0.0, 0.0, -1.0, 0)
}

extern "C" fn place_coordinate(this: Id, _: Sel) -> Coordinate {
    Coordinate { latitude: location_value(this, "latitude", 0.0), longitude: location_value(this, "longitude", 0.0) }
}

extern "C" fn place_altitude(this: Id, _: Sel) -> f64 {
    location_value(this, "altitude", 0.0)
}

extern "C" fn place_horizontal_accuracy(this: Id, _: Sel) -> f64 {
    location_value(this, "horizontalAccuracy", -1.0)
}

extern "C" fn place_vertical_accuracy(this: Id, _: Sel) -> f64 {
    location_value(this, "verticalAccuracy", -1.0)
}

/// Course and speed: -1, "not known", as for a location made by hand.
extern "C" fn place_unknown(_this: Id, _: Sel) -> f64 {
    -1.0
}

extern "C" fn place_timestamp(this: Id, _: Sel) -> Id {
    get_id(this, "timestamp")
}

/// Great-circle distance in meters.
extern "C" fn place_distance(this: Id, _: Sel, other: Id) -> f64 {
    let (a, b) = (place_coordinate(this, 0), place_coordinate(other, 0));
    let (la1, la2) = (a.latitude.to_radians(), b.latitude.to_radians());
    let (dla, dlo) = (la2 - la1, (b.longitude - a.longitude).to_radians());
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.sqrt().asin()
}

extern "C" fn place_description(this: Id, _: Sel) -> Id {
    let at = place_coordinate(this, 0);
    crate::fw::string::make(&format!(
        "<{:+.8},{:+.8}> +/- {:.2}m (speed -1.00 mps / course -1.00)",
        at.latitude,
        at.longitude,
        place_horizontal_accuracy(this, 0)
    ))
}

pub fn register(r: &mut Registry) {
    let store = define_class("ACAccountStore", "NSObject", 8);
    methods(
        store,
        &[
            ("accountTypeWithAccountTypeIdentifier:", account_type_for_identifier as usize),
            ("accountsWithAccountType:", no_accounts as usize),
            ("accounts", all_accounts as usize),
            ("accountWithIdentifier:", nil_with_id as usize),
            ("requestAccessToAccountsWithType:options:completion:", request_access_with_options as usize),
            ("requestAccessToAccountsWithType:withCompletionHandler:", request_access as usize),
        ],
    );
    let account_kind = define_class("ACAccountType", "NSObject", 8);
    methods(
        account_kind,
        &[("identifier", get_identifier as usize), ("accessGranted", no as usize), ("accountTypeDescription", get_identifier as usize)],
    );
    let account = define_class("ACAccount", "NSObject", 8);
    methods(
        account,
        &[
            ("initWithAccountType:", account_init_with_type as usize),
            ("accountType", account_type as usize),
            ("setAccountType:", ignore_id as usize),
            ("credential", account_credential as usize),
            ("setCredential:", account_set_credential as usize),
            ("username", account_username as usize),
            ("setUsername:", account_set_username as usize),
            ("identifier", get_identifier as usize),
        ],
    );

    let ads = define_class("ASIdentifierManager", "NSObject", 8);
    class_methods(ads, &[("sharedManager", shared_identifier_manager as usize)]);
    methods(ads, &[("advertisingIdentifier", advertising_identifier as usize), ("isAdvertisingTrackingEnabled", no as usize)]);

    let tweet = define_class("TWTweetComposeViewController", "UIViewController", 8);
    class_methods(tweet, &[("canSendTweet", class_no as usize)]);
    methods(
        tweet,
        &[
            ("setInitialText:", yes_with_id as usize),
            ("addURL:", yes_with_id as usize),
            ("addImage:", yes_with_id as usize),
            ("removeAllURLs", yes as usize),
            ("removeAllImages", yes as usize),
            ("completionHandler", tweet_completion as usize),
            ("setCompletionHandler:", tweet_set_completion as usize),
            ("gamebridgeAnswerShown", tweet_answer_shown as usize),
        ],
    );

    let place = define_class("CLLocation", "NSObject", 8);
    methods(
        place,
        &[
            ("initWithLatitude:longitude:", place_init as usize),
            ("initWithCoordinate:altitude:horizontalAccuracy:verticalAccuracy:timestamp:", place_init_full as usize),
            ("coordinate", place_coordinate as usize),
            ("altitude", place_altitude as usize),
            ("horizontalAccuracy", place_horizontal_accuracy as usize),
            ("verticalAccuracy", place_vertical_accuracy as usize),
            ("course", place_unknown as usize),
            ("speed", place_unknown as usize),
            ("timestamp", place_timestamp as usize),
            ("distanceFromLocation:", place_distance as usize),
            ("getDistanceFrom:", place_distance as usize),
            ("description", place_description as usize),
        ],
    );

    let banner = define_class("ADBannerView", "UIView", 16);
    class_methods(banner, &[("sizeFromBannerContentSizeIdentifier:", banner_size_for_identifier as usize)]);
    methods(
        banner,
        &[
            ("initWithFrame:", banner_init_with_frame as usize),
            ("initWithAdType:", banner_init_with_type as usize),
            ("adType", banner_type as usize),
            ("delegate", delegate as usize),
            ("setDelegate:", banner_set_delegate as usize),
            ("isBannerLoaded", no as usize),
            ("isBannerViewActionInProgress", no as usize),
            ("cancelBannerViewAction", nothing as usize),
            ("setRequiredContentSizeIdentifiers:", ignore_id as usize),
            ("requiredContentSizeIdentifiers", nil as usize),
            ("setCurrentContentSizeIdentifier:", ignore_id as usize),
            ("currentContentSizeIdentifier", nil as usize),
            ("advertisingSection", nil as usize),
            ("setAdvertisingSection:", ignore_id as usize),
        ],
    );
    let interstitial = define_class("ADInterstitialAd", "NSObject", 8);
    methods(
        interstitial,
        &[
            ("delegate", delegate as usize),
            ("setDelegate:", interstitial_set_delegate as usize),
            ("isLoaded", no as usize),
            ("isActionInProgress", no as usize),
            ("cancelAction", nothing as usize),
            ("presentFromViewController:", ignore_id as usize),
            ("presentInView:", no_with_id as usize),
        ],
    );

    let mail = define_class("MFMailComposeViewController", "UINavigationController", 8);
    class_methods(mail, &[("canSendMail", class_no as usize)]);
    methods(
        mail,
        &[
            ("mailComposeDelegate", mail_delegate as usize),
            ("setMailComposeDelegate:", mail_set_delegate as usize),
            ("setSubject:", ignore_id as usize),
            ("setToRecipients:", ignore_id as usize),
            ("setCcRecipients:", ignore_id as usize),
            ("setBccRecipients:", ignore_id as usize),
            ("setMessageBody:isHTML:", ignore_body_html as usize),
            ("addAttachmentData:mimeType:fileName:", ignore_id_id_id as usize),
            ("gamebridgeAnswerShown", mail_answer_shown as usize),
        ],
    );
    let message = define_class("MFMessageComposeViewController", "UINavigationController", 8);
    class_methods(
        message,
        &[
            ("canSendText", class_no as usize),
            ("canSendSubject", class_no as usize),
            ("canSendAttachments", class_no as usize),
        ],
    );
    methods(
        message,
        &[
            ("messageComposeDelegate", message_delegate as usize),
            ("setMessageComposeDelegate:", message_set_delegate as usize),
            ("setBody:", ignore_id as usize),
            ("setRecipients:", ignore_id as usize),
            ("setSubject:", ignore_id as usize),
            ("gamebridgeAnswerShown", message_answer_shown as usize),
        ],
    );

    let location = define_class("CLLocationManager", "NSObject", 8);
    class_methods(
        location,
        &[
            ("locationServicesEnabled", class_no as usize),
            ("authorizationStatus", class_authorization as usize),
            ("headingAvailable", class_no as usize),
            ("significantLocationChangeMonitoringAvailable", class_no as usize),
            ("regionMonitoringAvailable", class_no as usize),
            ("regionMonitoringEnabled", class_no as usize),
            ("deferredLocationUpdatesAvailable", class_no as usize),
            ("isRangingAvailable", class_no as usize),
            ("isMonitoringAvailableForClass:", class_monitoring_available as usize),
        ],
    );
    methods(
        location,
        &[
            ("init", location_init as usize),
            ("delegate", delegate as usize),
            ("setDelegate:", location_set_delegate as usize),
            ("desiredAccuracy", location_accuracy as usize),
            ("setDesiredAccuracy:", location_set_accuracy as usize),
            ("distanceFilter", location_distance as usize),
            ("setDistanceFilter:", location_set_distance as usize),
            ("headingFilter", location_heading_filter as usize),
            ("setHeadingFilter:", location_set_heading_filter as usize),
            ("purpose", location_purpose as usize),
            ("setPurpose:", location_set_purpose as usize),
            ("location", nil as usize),
            ("heading", nil as usize),
            ("locationServicesEnabled", no as usize),
            ("headingAvailable", no as usize),
            ("startUpdatingLocation", location_start as usize),
            ("startMonitoringSignificantLocationChanges", location_start as usize),
            ("requestLocation", location_start as usize),
            ("stopUpdatingLocation", nothing as usize),
            ("stopMonitoringSignificantLocationChanges", nothing as usize),
            // The heading isn't available: starting it does nothing, as on iOS.
            ("startUpdatingHeading", nothing as usize),
            ("stopUpdatingHeading", nothing as usize),
            ("dismissHeadingCalibrationDisplay", nothing as usize),
            // The status is already decided (denied), so asking does nothing.
            ("requestWhenInUseAuthorization", nothing as usize),
            ("requestAlwaysAuthorization", nothing as usize),
            ("setPausesLocationUpdatesAutomatically:", ignore_bool as usize),
            ("setAllowsBackgroundLocationUpdates:", ignore_bool as usize),
            ("setActivityType:", ignore_int as usize),
            ("setHeadingOrientation:", ignore_int as usize),
            ("startMonitoringForRegion:", ignore_id as usize),
            ("stopMonitoringForRegion:", ignore_id as usize),
            ("monitoredRegions", nil as usize),
            ("allowDeferredLocationUpdatesUntilTraveled:timeout:", ignore_two_doubles as usize),
        ],
    );
    let _ = ignore_double;
    // CLLocationAccuracy values (meters; negative ones are special).
    for (name, value) in [
        ("kCLLocationAccuracyBestForNavigation", -2.0f64),
        ("kCLLocationAccuracyBest", -1.0),
        ("kCLLocationAccuracyNearestTenMeters", 10.0),
        ("kCLLocationAccuracyHundredMeters", 100.0),
        ("kCLLocationAccuracyKilometer", 1000.0),
        ("kCLLocationAccuracyThreeKilometers", 3000.0),
        ("kCLDistanceFilterNone", -1.0),
        ("kCLHeadingFilterNone", -1.0),
    ] {
        r.data(name, leak(value));
    }
    r.data("kCLErrorDomain", leak(crate::fw::string::make("kCLErrorDomain")));

    let safari = define_class("SFSafariViewController", "UIViewController", 8);
    methods(
        safari,
        &[
            ("initWithURL:", safari_init as usize),
            ("initWithURL:entersReaderIfAvailable:", safari_init_reader as usize),
            ("delegate", delegate as usize),
            ("setDelegate:", safari_set_delegate as usize),
            ("setPreferredBarTintColor:", ignore_id as usize),
            ("setPreferredControlTintColor:", ignore_id as usize),
            ("gamebridgeAnswerShown", safari_answer_shown as usize),
        ],
    );
}

extern "C" fn ignore_two_doubles(_this: Id, _: Sel, _a: f64, _b: f64) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::{collections, string};
    use std::sync::Mutex;

    #[repr(C)]
    struct Block {
        header: crate::libsystem::mach::BlockHeader,
        heard: Mutex<Option<(bool, Id)>>,
    }

    extern "C" fn heard(block: usize, granted: bool, error: Id) {
        unsafe { *(*(block as *const Block)).heard.lock().unwrap() = Some((granted, error)) };
    }

    /// No accounts, no ad identifier, no tweets: Apple's own answers for a
    /// phone without them, the access request answered (denied) through
    /// its completion handler.
    #[test]
    fn services_give_apples_answers_for_none() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let store = runtime::send(runtime::send(runtime::class_named("ACAccountStore"), "alloc", &[]), "init", &[]);
        let twitter = runtime::send(store, "accountTypeWithAccountTypeIdentifier:", &[string::make("com.apple.twitter")]);
        assert_ne!(twitter, 0);
        assert_eq!(string::get(runtime::send(twitter, "identifier", &[])), "com.apple.twitter");
        assert_eq!(runtime::send(twitter, "accessGranted", &[]) & 0xFF, 0);
        let accounts = runtime::send(store, "accountsWithAccountType:", &[twitter]);
        assert!(collections::is_array(accounts) && collections::array_items(accounts).is_empty());
        let descriptor: [usize; 4] = [0, std::mem::size_of::<Block>(), 0, 0];
        let block = Block {
            header: crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: heard as usize, descriptor: &descriptor },
            heard: Mutex::new(None),
        };
        runtime::send(store, "requestAccessToAccountsWithType:options:completion:", &[twitter, 0, &block as *const Block as usize]);
        let start = std::time::Instant::now();
        while block.heard.lock().unwrap().is_none() && start.elapsed() < std::time::Duration::from_millis(500) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (granted, error) = block.heard.lock().unwrap().expect("the completion handler was never called");
        assert!(!granted);
        assert_eq!(string::get(runtime::send(error, "domain", &[])), "com.apple.accounts");
        assert_eq!(runtime::send(error, "code", &[]), 6, "ACErrorAccountNotFound");

        let ads = runtime::send(runtime::class_named("ASIdentifierManager"), "sharedManager", &[]);
        assert_eq!(runtime::send(ads, "isAdvertisingTrackingEnabled", &[]) & 0xFF, 0);
        let id = runtime::send(runtime::send(ads, "advertisingIdentifier", &[]), "UUIDString", &[]);
        assert_eq!(string::get(id), "00000000-0000-0000-0000-000000000000");
        assert_eq!(runtime::send(runtime::class_named("TWTweetComposeViewController"), "canSendTweet", &[]) & 0xFF, 0);
    }
}
