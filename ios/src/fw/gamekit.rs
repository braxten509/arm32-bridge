//! Game Center (GameKit). There is no Game Center here: the local player is
//! never signed in, and every request fails the way it fails on an offline
//! iPhone (GKErrorDomain, GKErrorCommunicationsFailure), so games carry on
//! without it.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::fw::uikit::{property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

const COMMUNICATIONS_FAILURE: isize = 3;

fn offline_error() -> Id {
    crate::fw::misc::error("GKErrorDomain", COMMUNICATIONS_FAILURE, "The requested operation could not be completed because Game Center is not available.")
}

/// Calls a completion block later on the main run loop with the given
/// arguments (block, then up to three object arguments; a block that takes
/// fewer ignores the rest).
fn answer_later(handler: usize, arguments: [Id; 3]) {
    if handler == 0 {
        return;
    }
    let handler = crate::libsystem::mach::block_copy(handler as *mut _) as usize;
    static PENDING: std::sync::Mutex<Vec<(usize, [Id; 3])>> = std::sync::Mutex::new(Vec::new());
    PENDING.lock().unwrap().push((handler, arguments));
    let block = crate::fw::media::make_callback(handler, |handler| {
        let arguments = {
            let mut pending = PENDING.lock().unwrap();
            let index = pending.iter().position(|(h, _)| *h == handler).expect("pending Game Center answer");
            pending.remove(index).1
        };
        let header = handler as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, Id, Id, Id) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(handler, arguments[0], arguments[1], arguments[2]);
    });
    post(Work::Block(block));
}

extern "C" fn local_player(_: Class, _: Sel) -> Id {
    static PLAYER: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *PLAYER.get_or_init(|| runtime::alloc_instance(runtime::class_named("GKLocalPlayer")))
}

extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}

extern "C" fn nothing(_this: Id, _: Sel) -> Id {
    0
}

/// setAuthenticateHandler: (iOS 6+): handler(viewController, error).
extern "C" fn set_authenticate_handler(_this: Id, _: Sel, handler: usize) {
    crate::log!("Game Center: sign-in asked for; answering 'not available'");
    answer_later(handler, [0, offline_error(), 0]);
}

/// Methods whose block takes only an error, or (result, error) with no result.
extern "C" fn error_only(_this: Id, _: Sel, handler: usize) {
    answer_later(handler, [offline_error(), 0, 0]);
}

extern "C" fn nothing_then_error(_this: Id, _: Sel, handler: usize) {
    answer_later(handler, [0, offline_error(), 0]);
}

extern "C" fn class_nothing_then_error(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [0, offline_error(), 0]);
}

extern "C" fn class_error_only(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [offline_error(), 0, 0]);
}

/// +reportAchievements:withCompletionHandler: / +reportScores:withCompletionHandler:
extern "C" fn class_report_error_only(_: Class, _: Sel, _items: Id, handler: usize) {
    answer_later(handler, [offline_error(), 0, 0]);
}

/// loadCategoriesWithCompletionHandler: handler(categories, titles, error).
extern "C" fn class_two_nothings_then_error(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [0, 0, offline_error()]);
}

extern "C" fn ignore(_this: Id, _: Sel, _value: Id) {}

extern "C" fn ignore_none(_this: Id, _: Sel) {}

/// init...: methods that keep their one object argument (an identifier).
extern "C" fn init_with_identifier(this: Id, _: Sel, identifier: Id) -> Id {
    property_set(this, "identifier", identifier as u64);
    this
}

extern "C" fn identifier(this: Id, _: Sel) -> Id {
    property_get(this, "identifier").unwrap_or(0) as Id
}

extern "C" fn set_identifier(this: Id, _: Sel, identifier: Id) {
    property_set(this, "identifier", identifier as u64);
}

extern "C" fn percent_complete(this: Id, _: Sel) -> f64 {
    property_get(this, "percentComplete").map(f64::from_bits).unwrap_or(0.0)
}

extern "C" fn set_percent_complete(this: Id, _: Sel, percent: f64) {
    property_set(this, "percentComplete", percent.to_bits());
}

extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}

extern "C" fn score_value(this: Id, _: Sel) -> i64 {
    property_get(this, "value").unwrap_or(0) as i64
}

extern "C" fn set_score_value(this: Id, _: Sel, value: i64) {
    property_set(this, "value", value as u64);
}

extern "C" fn ignore_u64(_this: Id, _: Sel, _value: u64) {}

pub fn register(_r: &mut Registry) {
    define_class("GKPlayer", "NSObject", 8);
    let local = define_class("GKLocalPlayer", "GKPlayer", 8);
    class_methods(local, &[("localPlayer", local_player as usize)]);
    methods(
        local,
        &[
            ("isAuthenticated", no as usize),
            ("isUnderage", no as usize),
            ("playerID", nothing as usize),
            ("alias", nothing as usize),
            ("displayName", nothing as usize),
            ("authenticateHandler", nothing as usize),
            ("setAuthenticateHandler:", set_authenticate_handler as usize),
            ("authenticateWithCompletionHandler:", error_only as usize),
            ("loadFriendsWithCompletionHandler:", nothing_then_error as usize),
            ("loadFriendPlayersWithCompletionHandler:", nothing_then_error as usize),
            ("loadDefaultLeaderboardIdentifierWithCompletionHandler:", nothing_then_error as usize),
            ("registerListener:", ignore as usize),
            ("unregisterListener:", ignore as usize),
            ("unregisterAllListeners", ignore_none as usize),
        ],
    );

    let achievement = define_class("GKAchievement", "NSObject", 8);
    class_methods(
        achievement,
        &[
            ("loadAchievementsWithCompletionHandler:", class_nothing_then_error as usize),
            ("resetAchievementsWithCompletionHandler:", class_error_only as usize),
            ("reportAchievements:withCompletionHandler:", class_report_error_only as usize),
        ],
    );
    methods(
        achievement,
        &[
            ("reportAchievementWithCompletionHandler:", error_only as usize),
            ("initWithIdentifier:", init_with_identifier as usize),
            ("identifier", identifier as usize),
            ("setIdentifier:", set_identifier as usize),
            ("percentComplete", percent_complete as usize),
            ("setPercentComplete:", set_percent_complete as usize),
            ("isCompleted", no as usize),
            ("showsCompletionBanner", no as usize),
            ("setShowsCompletionBanner:", ignore_bool as usize),
        ],
    );
    let description = define_class("GKAchievementDescription", "NSObject", 8);
    class_methods(description, &[("loadAchievementDescriptionsWithCompletionHandler:", class_nothing_then_error as usize)]);

    let score = define_class("GKScore", "NSObject", 8);
    class_methods(score, &[("reportScores:withCompletionHandler:", class_report_error_only as usize)]);
    methods(
        score,
        &[
            ("reportScoreWithCompletionHandler:", error_only as usize),
            ("initWithCategory:", init_with_identifier as usize),
            ("initWithLeaderboardIdentifier:", init_with_identifier as usize),
            ("category", identifier as usize),
            ("setCategory:", set_identifier as usize),
            ("leaderboardIdentifier", identifier as usize),
            ("setLeaderboardIdentifier:", set_identifier as usize),
            ("value", score_value as usize),
            ("setValue:", set_score_value as usize),
            ("setContext:", ignore_u64 as usize),
            ("setShouldSetDefaultLeaderboard:", ignore_bool as usize),
        ],
    );
    let leaderboard = define_class("GKLeaderboard", "NSObject", 8);
    class_methods(
        leaderboard,
        &[
            ("loadLeaderboardsWithCompletionHandler:", class_nothing_then_error as usize),
            ("loadCategoriesWithCompletionHandler:", class_two_nothings_then_error as usize),
        ],
    );
    methods(leaderboard, &[("loadScoresWithCompletionHandler:", nothing_then_error as usize)]);
}
