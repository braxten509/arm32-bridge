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
    let imp = lock().lookup(runtime::class_named("UIView"), s).expect("UIView initWithFrame:");
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

/// A new delegate hears the (denied) authorization, as on iOS.
extern "C" fn location_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
    if delegate == 0 {
        return;
    }
    later(move || {
        if get_id(this, "delegate") == delegate {
            tell(delegate, "locationManager:didChangeAuthorizationStatus:", &[this, AUTHORIZATION_DENIED as usize]);
        }
    });
}

/// Starting updates fails with kCLErrorDenied.
extern "C" fn location_start(this: Id, _: Sel) {
    crate::log!("location asked for: location services are off here, telling the game it was denied");
    later(move || {
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

pub fn register(r: &mut Registry) {
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
