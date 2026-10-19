//! Game Center (GameKit). There is no Game Center here: the local player is
//! never signed in, and every request fails the way it fails on an offline
//! iPhone (GKErrorDomain, GKErrorCommunicationsFailure), so games carry on
//! without it.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

const COMMUNICATIONS_FAILURE: isize = 3;

fn offline_error() -> Id {
    crate::fw::misc::error("GKErrorDomain", COMMUNICATIONS_FAILURE, "The requested operation could not be completed because Game Center is not available.")
}

/// Calls a completion block later on the main run loop with the given
/// arguments (block, then up to two object arguments).
fn answer_later(handler: usize, arguments: [Id; 2]) {
    if handler == 0 {
        return;
    }
    let handler = crate::libsystem::mach::block_copy(handler as *mut _) as usize;
    static PENDING: std::sync::Mutex<Vec<(usize, [Id; 2])>> = std::sync::Mutex::new(Vec::new());
    PENDING.lock().unwrap().push((handler, arguments));
    let block = crate::fw::media::make_callback(handler, |handler| {
        let arguments = {
            let mut pending = PENDING.lock().unwrap();
            let index = pending.iter().position(|(h, _)| *h == handler).expect("pending Game Center answer");
            pending.remove(index).1
        };
        let header = handler as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, Id, Id) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(handler, arguments[0], arguments[1]);
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
    answer_later(handler, [0, offline_error()]);
}

/// Methods whose block takes only an error, or (result, error) with no result.
extern "C" fn error_only(_this: Id, _: Sel, handler: usize) {
    answer_later(handler, [offline_error(), 0]);
}

extern "C" fn nothing_then_error(_this: Id, _: Sel, handler: usize) {
    answer_later(handler, [0, offline_error()]);
}

extern "C" fn class_nothing_then_error(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [0, offline_error()]);
}

extern "C" fn class_error_only(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [offline_error(), 0]);
}

/// +reportAchievements:withCompletionHandler: / +reportScores:withCompletionHandler:
extern "C" fn class_report_error_only(_: Class, _: Sel, _items: Id, handler: usize) {
    answer_later(handler, [offline_error(), 0]);
}

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
    methods(achievement, &[("reportAchievementWithCompletionHandler:", error_only as usize)]);
    let description = define_class("GKAchievementDescription", "NSObject", 8);
    class_methods(description, &[("loadAchievementDescriptionsWithCompletionHandler:", class_nothing_then_error as usize)]);

    let score = define_class("GKScore", "NSObject", 8);
    class_methods(score, &[("reportScores:withCompletionHandler:", class_report_error_only as usize)]);
    methods(score, &[("reportScoreWithCompletionHandler:", error_only as usize)]);
    let leaderboard = define_class("GKLeaderboard", "NSObject", 8);
    class_methods(leaderboard, &[("loadLeaderboardsWithCompletionHandler:", class_nothing_then_error as usize)]);
    methods(leaderboard, &[("loadScoresWithCompletionHandler:", nothing_then_error as usize)]);
}
