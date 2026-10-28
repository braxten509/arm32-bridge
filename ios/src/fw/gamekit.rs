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

/// GKErrorDomain: the exported constant and every Game Center error's domain.
fn error_domain() -> Id {
    static DOMAIN: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *DOMAIN.get_or_init(|| crate::fw::string::make("GKErrorDomain"))
}

fn offline_error() -> Id {
    crate::fw::misc::error_in(error_domain(), COMMUNICATIONS_FAILURE, "The requested operation could not be completed because Game Center is not available.")
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
        // The answer's own copy goes once it has run.
        release_block(handler);
    });
    post(Work::Block(block));
}

/// _Block_release (libsystem's, by its exported name).
fn release_block(block: usize) {
    if block == 0 {
        return;
    }
    if let Some((address, _)) = crate::symbols::lookup("__Block_release") {
        let release: extern "C" fn(usize) = unsafe { std::mem::transmute(address) };
        release(block);
    }
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

/// The local player's authenticateHandler (a copy), kept as iOS 6 keeps it.
/// Read and copied under this lock, so another thread setting a new one
/// cannot let it go in between (Apple's atomic copy property).
static AUTHENTICATE_HANDLER: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// setAuthenticateHandler: (iOS 6+): handler(viewController, error). It is
/// kept (the getter gives it back) and asked again each time the game comes
/// back to the foreground, as GameKit does while nobody is signed in.
extern "C" fn set_authenticate_handler(_this: Id, _: Sel, handler: usize) {
    crate::log!("Game Center: sign-in asked for; answering 'not available'");
    let kept = if handler == 0 { 0 } else { crate::libsystem::mach::block_copy(handler as *mut _) as usize };
    let replaced = {
        let mut current = AUTHENTICATE_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
        answer_later(kept, [0, offline_error(), 0]);
        std::mem::replace(&mut *current, kept)
    };
    // The handler it replaces is let go, as Apple's copy property setter does
    // (an answer still waiting holds its own copy), outside the lock: its
    // dispose may run game code.
    release_block(replaced);
}

extern "C" fn authenticate_handler(_this: Id, _: Sel) -> usize {
    *AUTHENTICATE_HANDLER.lock().unwrap_or_else(|e| e.into_inner())
}

/// The game came back to the foreground: a kept authenticateHandler hears
/// again that nobody is signed in. Whether one was asked.
pub fn ask_authenticate_handler_again() -> bool {
    let handler = AUTHENTICATE_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    answer_later(*handler, [0, offline_error(), 0]);
    *handler != 0
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

// ----- Game Center screens, challenges, matches and voice chat -----

const NOT_AUTHENTICATED: isize = 6;

fn not_signed_in_error() -> Id {
    crate::fw::misc::error_in(error_domain(), NOT_AUTHENTICATED, "The requested operation could not be completed because local player has not been authenticated.")
}

macro_rules! object_property {
    ($get:ident, $set:ident, $key:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            property_get(this, $key).unwrap_or(0) as Id
        }
        extern "C" fn $set(this: Id, _: Sel, value: Id) {
            property_set(this, $key, value as u64);
        }
    };
}
macro_rules! number_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> isize {
            property_get(this, $key).map_or($default, |v| v as isize)
        }
        extern "C" fn $set(this: Id, _: Sel, value: isize) {
            property_set(this, $key, value as u64);
        }
    };
}

object_property!(gc_delegate, gc_set_delegate, "gameCenterDelegate");
object_property!(achievements_delegate, achievements_set_delegate, "achievementDelegate");
object_property!(leaderboard_delegate, leaderboard_set_delegate, "leaderboardDelegate");
object_property!(matchmaker_delegate, matchmaker_set_delegate, "matchmakerDelegate");
object_property!(compose_delegate, compose_set_delegate, "composeViewDelegate");
object_property!(category, set_category, "category");
object_property!(match_request, set_match_request, "matchRequest");
number_property!(view_state, set_view_state, "viewState", 0);
number_property!(time_scope, set_time_scope, "timeScope", 2);

/// Shown, a Game Center screen does what iOS does once the player closes it
/// (offline it only says Game Center is unavailable): the delegate hears
/// it finished.
fn finish_screen(this: Id, delegate_key: &'static str, message: &str) {
    crate::log!("Game Center screen shown: not available here, telling the game it was closed");
    let delegate = property_get(this, delegate_key).unwrap_or(0) as Id;
    if delegate != 0 && runtime::responds(delegate, message) {
        runtime::send(delegate, message, &[this]);
    }
}

extern "C" fn gc_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "gameCenterDelegate", "gameCenterViewControllerDidFinish:");
}
extern "C" fn achievements_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "achievementDelegate", "achievementViewControllerDidFinish:");
}
extern "C" fn leaderboard_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "leaderboardDelegate", "leaderboardViewControllerDidFinish:");
}
extern "C" fn friend_request_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "composeViewDelegate", "friendRequestComposeViewControllerDidFinish:");
}
/// The matchmaker screen is cancelled (its delegate must handle that one).
extern "C" fn matchmaker_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "matchmakerDelegate", "matchmakerViewControllerWasCancelled:");
}

extern "C" fn matchmaker_init_with_request(this: Id, _: Sel, request: Id) -> Id {
    property_set(this, "matchRequest", request as u64);
    this
}

extern "C" fn class_maximum_players(_: Class, _: Sel, kind: usize) -> usize {
    // GKMatchTypePeerToPeer 4, GKMatchTypeHosted 16, GKMatchTypeTurnBased 16.
    if kind == 0 { 4 } else { 16 }
}

number_property!(request_min, request_set_min, "minPlayers", 2);
number_property!(request_max, request_set_max, "maxPlayers", 4);
number_property!(request_default, request_set_default, "defaultNumberOfPlayers", 2);
number_property!(request_group, request_set_group, "playerGroup", 0);
number_property!(request_attributes, request_set_attributes, "playerAttributes", 0);
object_property!(request_invitees, request_set_invitees, "playersToInvite");
object_property!(request_recipients, request_set_recipients, "recipients");
object_property!(request_message, request_set_message, "inviteMessage");

extern "C" fn shared_matchmaker(_: Class, _: Sel) -> Id {
    static SHARED: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *SHARED.get_or_init(|| runtime::alloc_instance(runtime::class_named("GKMatchmaker")))
}

object_property!(invite_handler, set_invite_handler_raw, "inviteHandler");

extern "C" fn set_invite_handler(this: Id, s: Sel, handler: usize) {
    // Kept, never called: no invitations can arrive. A copy property: the
    // handler it replaces is let go of.
    let handler = if handler != 0 { crate::libsystem::mach::block_copy(handler as *mut _) as usize } else { 0 };
    let old = invite_handler(this, s);
    set_invite_handler_raw(this, s, handler);
    release_block(old);
}

/// Matchmaking requests fail at once: the player isn't signed in.
extern "C" fn one_object_then_signed_out(_this: Id, _: Sel, _request: Id, handler: usize) {
    answer_later(handler, [0, not_signed_in_error(), 0]);
}
extern "C" fn two_objects_then_signed_out(_this: Id, _: Sel, _a: Id, _b: Id, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}
extern "C" fn count_then_signed_out(_this: Id, _: Sel, handler: usize) {
    answer_later(handler, [0, not_signed_in_error(), 0]);
}
extern "C" fn group_count_then_signed_out(_this: Id, _: Sel, _group: usize, handler: usize) {
    answer_later(handler, [0, not_signed_in_error(), 0]);
}
extern "C" fn class_one_object_then_signed_out(_: Class, _: Sel, _request: Id, handler: usize) {
    answer_later(handler, [0, not_signed_in_error(), 0]);
}
extern "C" fn class_signed_out(_: Class, _: Sel, handler: usize) {
    answer_later(handler, [0, not_signed_in_error(), 0]);
}

/// GKNotificationBanner: nothing is drawn, so the banner is over at once
/// and its completion handler runs.
extern "C" fn banner_show(_: Class, _: Sel, title: Id, message: Id, handler: usize) {
    crate::fw::controls::not_drawn(&format!(
        "Game Center banner \"{}\" / \"{}\"",
        crate::fw::string::describe(title),
        crate::fw::string::describe(message)
    ));
    answer_later(handler, [0, 0, 0]);
}
extern "C" fn banner_show_duration(class: Class, s: Sel, title: Id, message: Id, _duration: f64, handler: usize) {
    banner_show(class, s, title, message, handler);
}

extern "C" fn challenge_handler(_: Class, _: Sel) -> Id {
    static SHARED: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *SHARED.get_or_init(|| runtime::alloc_instance(runtime::class_named("GKChallengeEventHandler")))
}

object_property!(handler_delegate, handler_set_delegate, "delegate");

extern "C" fn zero_state(_this: Id, _: Sel) -> isize {
    0
}

extern "C" fn voip_allowed(_: Class, _: Sel) -> bool {
    false
}

extern "C" fn default_voice_service(_: Class, _: Sel) -> Id {
    static SHARED: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *SHARED.get_or_init(|| runtime::alloc_instance(runtime::class_named("GKVoiceChatService")))
}

object_property!(voice_client, voice_set_client, "client");

/// startVoiceChatWithParticipantID:error: says NO with the error.
extern "C" fn voice_start(_this: Id, _: Sel, _participant: Id, error: *mut Id) -> bool {
    if !error.is_null() {
        unsafe { *error = crate::fw::misc::error("GKVoiceChatServiceErrorDomain", 32005, "Voice chat is not available.") };
    }
    false
}

extern "C" fn ignore_two(_this: Id, _: Sel, _a: Id, _b: Id) {}
extern "C" fn ignore_float(_this: Id, _: Sel, _value: f32) {}
extern "C" fn zero_float(_this: Id, _: Sel) -> f32 {
    0.0
}

// ----- turn-based matches -----

object_property!(turn_matchmaker_delegate, turn_matchmaker_set_delegate, "turnBasedMatchmakerDelegate");
number_property!(show_existing_matches, set_show_existing_matches, "showExistingMatches", 1);
number_property!(participant_outcome, participant_set_outcome, "matchOutcome", 0);

extern "C" fn turn_based_event_handler(_: Class, _: Sel) -> Id {
    static SHARED: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *SHARED.get_or_init(|| runtime::alloc_instance(runtime::class_named("GKTurnBasedEventHandler")))
}

/// Turn changes can't reach Game Center: the handler hears the player isn't
/// signed in (its only argument is the error).
extern "C" fn end_match_in_turn(_this: Id, _: Sel, _data: Id, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}
extern "C" fn end_turn(_this: Id, _: Sel, _next: Id, _data: Id, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}
extern "C" fn end_turn_with_timeout(_this: Id, _: Sel, _next: Id, _timeout: f64, _data: Id, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}
extern "C" fn quit_in_turn(_this: Id, _: Sel, _outcome: isize, _next: Id, _data: Id, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}
extern "C" fn quit_out_of_turn(_this: Id, _: Sel, _outcome: isize, handler: usize) {
    answer_later(handler, [not_signed_in_error(), 0, 0]);
}

/// The turn-based matchmaker screen is cancelled once shown.
extern "C" fn turn_matchmaker_answer_shown(this: Id, _: Sel) {
    finish_screen(this, "turnBasedMatchmakerDelegate", "turnBasedMatchmakerViewControllerWasCancelled:");
}

/// +placeholderCompletedAchievementImage: a plain gray medal (a filled
/// circle, 64 points) in place of Game Center's own picture.
extern "C" fn placeholder_achievement_image(_: Class, _: Sel) -> Id {
    use crate::fw::graphics::{make_image, Bitmap, Space, PREMULTIPLIED_LAST};
    const SIDE: usize = 64;
    let mut data = vec![0u8; SIDE * SIDE * 4];
    let middle = SIDE as f64 / 2.0;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (dx, dy) = (x as f64 + 0.5 - middle, y as f64 + 0.5 - middle);
            if dx * dx + dy * dy <= (middle - 1.0) * (middle - 1.0) {
                data[(y * SIDE + x) * 4..][..4].copy_from_slice(&[0x9a, 0x9a, 0x9a, 0xff]);
            }
        }
    }
    let picture = make_image(Bitmap { width: SIDE, height: SIDE, bits_per_pixel: 32, bytes_per_row: SIDE * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data });
    let image = runtime::send(runtime::class_named("UIImage"), "imageWithCGImage:", &[picture]);
    crate::fw::counted::release(picture);
    image
}

pub fn register(r: &mut Registry) {
    r.data("GKErrorDomain", crate::symbols::leak(error_domain()));
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
            ("authenticateHandler", authenticate_handler as usize),
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
    class_methods(runtime::class_named("GKPlayer"), &[("loadPlayersForIdentifiers:withCompletionHandler:", class_one_object_then_signed_out as usize)]);

    let gc = define_class("GKGameCenterViewController", "UINavigationController", 8);
    methods(
        gc,
        &[
            ("gameCenterDelegate", gc_delegate as usize),
            ("setGameCenterDelegate:", gc_set_delegate as usize),
            ("viewState", view_state as usize),
            ("setViewState:", set_view_state as usize),
            ("leaderboardTimeScope", time_scope as usize),
            ("setLeaderboardTimeScope:", set_time_scope as usize),
            ("leaderboardCategory", category as usize),
            ("setLeaderboardCategory:", set_category as usize),
            ("leaderboardIdentifier", category as usize),
            ("setLeaderboardIdentifier:", set_category as usize),
            ("gamebridgeAnswerShown", gc_answer_shown as usize),
        ],
    );
    let achievements = define_class("GKAchievementViewController", "GKGameCenterViewController", 8);
    methods(
        achievements,
        &[
            ("achievementDelegate", achievements_delegate as usize),
            ("setAchievementDelegate:", achievements_set_delegate as usize),
            ("gamebridgeAnswerShown", achievements_answer_shown as usize),
        ],
    );
    let leaderboards = define_class("GKLeaderboardViewController", "GKGameCenterViewController", 8);
    methods(
        leaderboards,
        &[
            ("leaderboardDelegate", leaderboard_delegate as usize),
            ("setLeaderboardDelegate:", leaderboard_set_delegate as usize),
            ("timeScope", time_scope as usize),
            ("setTimeScope:", set_time_scope as usize),
            ("category", category as usize),
            ("setCategory:", set_category as usize),
            ("gamebridgeAnswerShown", leaderboard_answer_shown as usize),
        ],
    );
    let friend_request = define_class("GKFriendRequestComposeViewController", "UINavigationController", 8);
    class_methods(friend_request, &[("maxNumberOfRecipients", class_max_recipients as usize)]);
    methods(
        friend_request,
        &[
            ("composeViewDelegate", compose_delegate as usize),
            ("setComposeViewDelegate:", compose_set_delegate as usize),
            ("setMessage:", ignore as usize),
            ("addRecipientsWithEmailAddresses:", ignore as usize),
            ("addRecipientsWithPlayerIDs:", ignore as usize),
            ("addRecipientPlayers:", ignore as usize),
            ("gamebridgeAnswerShown", friend_request_answer_shown as usize),
        ],
    );
    let matchmaker_screen = define_class("GKMatchmakerViewController", "UINavigationController", 8);
    methods(
        matchmaker_screen,
        &[
            ("initWithMatchRequest:", matchmaker_init_with_request as usize),
            ("initWithInvite:", matchmaker_init_with_request as usize),
            ("matchmakerDelegate", matchmaker_delegate as usize),
            ("setMatchmakerDelegate:", matchmaker_set_delegate as usize),
            ("matchRequest", match_request as usize),
            ("setHosted:", ignore_bool as usize),
            ("isHosted", no as usize),
            ("addPlayersToMatch:", ignore as usize),
            ("setHostedPlayer:connected:", ignore_two as usize),
            ("gamebridgeAnswerShown", matchmaker_answer_shown as usize),
        ],
    );
    let _ = set_match_request;

    let request = define_class("GKMatchRequest", "NSObject", 8);
    class_methods(request, &[("maxPlayersAllowedForMatchOfType:", class_maximum_players as usize)]);
    methods(
        request,
        &[
            ("minPlayers", request_min as usize),
            ("setMinPlayers:", request_set_min as usize),
            ("maxPlayers", request_max as usize),
            ("setMaxPlayers:", request_set_max as usize),
            ("defaultNumberOfPlayers", request_default as usize),
            ("setDefaultNumberOfPlayers:", request_set_default as usize),
            ("playerGroup", request_group as usize),
            ("setPlayerGroup:", request_set_group as usize),
            ("playerAttributes", request_attributes as usize),
            ("setPlayerAttributes:", request_set_attributes as usize),
            ("playersToInvite", request_invitees as usize),
            ("setPlayersToInvite:", request_set_invitees as usize),
            ("recipients", request_recipients as usize),
            ("setRecipients:", request_set_recipients as usize),
            ("inviteMessage", request_message as usize),
            ("setInviteMessage:", request_set_message as usize),
            ("setRecipientResponseHandler:", ignore as usize),
            ("setInviteeResponseHandler:", ignore as usize),
        ],
    );
    let matchmaker = define_class("GKMatchmaker", "NSObject", 8);
    class_methods(matchmaker, &[("sharedMatchmaker", shared_matchmaker as usize)]);
    methods(
        matchmaker,
        &[
            ("inviteHandler", invite_handler as usize),
            ("setInviteHandler:", set_invite_handler as usize),
            ("findMatchForRequest:withCompletionHandler:", one_object_then_signed_out as usize),
            ("findPlayersForHostedMatchRequest:withCompletionHandler:", one_object_then_signed_out as usize),
            ("findPlayersForHostedRequest:withCompletionHandler:", one_object_then_signed_out as usize),
            ("matchForInvite:completionHandler:", one_object_then_signed_out as usize),
            ("addPlayersToMatch:matchRequest:completionHandler:", two_objects_then_signed_out as usize),
            ("queryActivityWithCompletionHandler:", count_then_signed_out as usize),
            ("queryPlayerGroupActivity:withCompletionHandler:", group_count_then_signed_out as usize),
            ("cancel", ignore_none as usize),
            ("cancelInviteToPlayer:", ignore as usize),
            ("cancelPendingInviteToPlayer:", ignore as usize),
            ("finishMatchmakingForMatch:", ignore as usize),
            ("stopBrowsingForNearbyPlayers", ignore_none as usize),
        ],
    );

    let banner = define_class("GKNotificationBanner", "NSObject", 8);
    class_methods(
        banner,
        &[
            ("showBannerWithTitle:message:completionHandler:", banner_show as usize),
            ("showBannerWithTitle:message:duration:completionHandler:", banner_show_duration as usize),
        ],
    );

    let challenge = define_class("GKChallenge", "NSObject", 8);
    class_methods(challenge, &[("loadReceivedChallengesWithCompletionHandler:", class_signed_out as usize)]);
    methods(
        challenge,
        &[
            ("issuingPlayerID", nothing as usize),
            ("receivingPlayerID", nothing as usize),
            ("issuingPlayer", nothing as usize),
            ("receivingPlayer", nothing as usize),
            ("state", zero_state as usize),
            ("issueDate", nothing as usize),
            ("completionDate", nothing as usize),
            ("message", nothing as usize),
            ("decline", ignore_none as usize),
        ],
    );
    let score_challenge = define_class("GKScoreChallenge", "GKChallenge", 8);
    methods(score_challenge, &[("score", nothing as usize)]);
    let achievement_challenge = define_class("GKAchievementChallenge", "GKChallenge", 8);
    methods(achievement_challenge, &[("achievement", nothing as usize)]);
    let event_handler = define_class("GKChallengeEventHandler", "NSObject", 8);
    class_methods(event_handler, &[("challengeEventHandler", challenge_handler as usize)]);
    methods(event_handler, &[("delegate", handler_delegate as usize), ("setDelegate:", handler_set_delegate as usize)]);

    let turn_based = define_class("GKTurnBasedMatch", "NSObject", 8);
    class_methods(
        turn_based,
        &[
            ("loadMatchesWithCompletionHandler:", class_signed_out as usize),
            ("findMatchForRequest:withCompletionHandler:", class_one_object_then_signed_out as usize),
            ("loadMatchWithID:withCompletionHandler:", class_one_object_then_signed_out as usize),
        ],
    );
    methods(
        turn_based,
        &[
            ("matchID", nothing as usize),
            ("participants", nothing as usize),
            ("currentParticipant", nothing as usize),
            ("matchData", nothing as usize),
            ("message", nothing as usize),
            ("creationDate", nothing as usize),
            ("status", zero_state as usize),
            ("loadMatchDataWithCompletionHandler:", nothing_then_error as usize),
            ("removeWithCompletionHandler:", error_only as usize),
            ("endMatchInTurnWithMatchData:completionHandler:", end_match_in_turn as usize),
            ("saveCurrentTurnWithMatchData:completionHandler:", end_match_in_turn as usize),
            ("endTurnWithNextParticipant:matchData:completionHandler:", end_turn as usize),
            ("endTurnWithNextParticipants:turnTimeout:matchData:completionHandler:", end_turn_with_timeout as usize),
            ("participantQuitInTurnWithOutcome:nextParticipant:matchData:completionHandler:", quit_in_turn as usize),
            ("participantQuitOutOfTurnWithOutcome:withCompletionHandler:", quit_out_of_turn as usize),
        ],
    );
    let turn_handler = define_class("GKTurnBasedEventHandler", "NSObject", 8);
    class_methods(turn_handler, &[("sharedTurnBasedEventHandler", turn_based_event_handler as usize)]);
    methods(turn_handler, &[("delegate", handler_delegate as usize), ("setDelegate:", handler_set_delegate as usize)]);
    let turn_screen = define_class("GKTurnBasedMatchmakerViewController", "UINavigationController", 8);
    methods(
        turn_screen,
        &[
            ("initWithMatchRequest:", matchmaker_init_with_request as usize),
            ("turnBasedMatchmakerDelegate", turn_matchmaker_delegate as usize),
            ("setTurnBasedMatchmakerDelegate:", turn_matchmaker_set_delegate as usize),
            ("showExistingMatches", show_existing_matches as usize),
            ("setShowExistingMatches:", set_show_existing_matches as usize),
            ("gamebridgeAnswerShown", turn_matchmaker_answer_shown as usize),
        ],
    );
    class_methods(description, &[("placeholderCompletedAchievementImage", placeholder_achievement_image as usize)]);
    let participant = define_class("GKTurnBasedParticipant", "NSObject", 8);
    methods(
        participant,
        &[
            ("playerID", nothing as usize),
            ("player", nothing as usize),
            ("lastTurnDate", nothing as usize),
            ("timeoutDate", nothing as usize),
            ("status", zero_state as usize),
            ("matchOutcome", participant_outcome as usize),
            ("setMatchOutcome:", participant_set_outcome as usize),
        ],
    );

    let voice_chat = define_class("GKVoiceChat", "NSObject", 8);
    class_methods(voice_chat, &[("isVoIPAllowed", voip_allowed as usize)]);
    methods(
        voice_chat,
        &[
            ("start", ignore_none as usize),
            ("stop", ignore_none as usize),
            ("isActive", no as usize),
            ("setActive:", ignore_bool as usize),
            ("name", nothing as usize),
            ("volume", zero_float as usize),
            ("setVolume:", ignore_float as usize),
            ("setMute:forPlayer:", ignore_bool_id as usize),
            ("setPlayerStateUpdateHandler:", ignore as usize),
            ("playerIDs", nothing as usize),
        ],
    );
    let voice_service = define_class("GKVoiceChatService", "NSObject", 8);
    class_methods(
        voice_service,
        &[("defaultVoiceChatService", default_voice_service as usize), ("isVoIPAllowed", voip_allowed as usize)],
    );
    methods(
        voice_service,
        &[
            ("client", voice_client as usize),
            ("setClient:", voice_set_client as usize),
            ("startVoiceChatWithParticipantID:error:", voice_start as usize),
            ("stopVoiceChatWithParticipantID:", ignore as usize),
            ("acceptCallID:error:", voice_accept as usize),
            ("denyCallID:", ignore_u64 as usize),
            ("receivedData:fromParticipantID:", ignore_two as usize),
            ("receivedRealTimeData:fromParticipantID:", ignore_two as usize),
            ("setMicrophoneMuted:", ignore_bool as usize),
            ("isMicrophoneMuted", no as usize),
            ("setRemoteParticipantVolume:", ignore_float as usize),
            ("setOutputMeteringEnabled:", ignore_bool as usize),
            ("setInputMeteringEnabled:", ignore_bool as usize),
        ],
    );
}

extern "C" fn class_max_recipients(_: Class, _: Sel) -> usize {
    1
}

extern "C" fn ignore_bool_id(_this: Id, _: Sel, _flag: u8, _player: Id) {}

extern "C" fn voice_accept(_this: Id, _: Sel, _call: u64, error: *mut Id) -> bool {
    voice_start(0, 0, 0, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data symbol's object (games read the variable, often unchecked).
    fn constant(name: &str) -> Id {
        let (address, kind) = crate::symbols::lookup(&format!("_{name}")).unwrap_or_else(|| panic!("{name} is not exported"));
        assert_eq!(kind, crate::symbols::Kind::Data, "{name}");
        unsafe { *(address as *const Id) }
    }

    /// iOS 6's authenticateHandler is kept: the getter gives it back, and
    /// coming back to the foreground asks it again (still not signed in).
    #[test]
    fn the_authenticate_handler_is_kept_and_asked_again_on_return() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        extern "C" fn invoke(_block: usize, _view_controller: Id, _error: Id) {}
        let block: &'static crate::libsystem::mach::BlockHeader = Box::leak(Box::new(crate::libsystem::mach::BlockHeader {
            isa: 0,
            flags: 1 << 28,
            reserved: 0,
            invoke: invoke as usize,
            descriptor: std::ptr::null(),
        }));
        let handler = block as *const _ as usize;
        let player = runtime::send(runtime::class_named("GKLocalPlayer"), "localPlayer", &[]);
        runtime::send(player, "setAuthenticateHandler:", &[handler]);
        assert_eq!(runtime::send(player, "authenticateHandler", &[]), handler);
        assert!(ask_authenticate_handler_again(), "asked again on return");
    }

    /// Games compare a sign-in error's domain with GKErrorDomain, and the
    /// Accounts keys are read the same way: none may be left unexported.
    #[test]
    fn error_domain_and_account_keys_are_exported_strings() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let domain = constant("GKErrorDomain");
        assert_eq!(crate::fw::string::get(domain), "GKErrorDomain");
        assert_eq!(runtime::send(offline_error(), "domain", &[]), domain, "the error's domain is the constant itself");
        assert_eq!(runtime::send(offline_error(), "code", &[]), COMMUNICATIONS_FAILURE as usize);
        for (name, value) in [
            ("ACFacebookAppIdKey", "ACFacebookAppIdKey"),
            ("ACFacebookPermissionsKey", "ACFacebookPermissionsKey"),
            ("ACFacebookAudienceKey", "ACFacebookAudienceKey"),
            ("ACFacebookAudienceEveryone", "everyone"),
            ("ACFacebookAudienceFriends", "friends"),
            ("ACFacebookAudienceOnlyMe", "me"),
        ] {
            assert_eq!(crate::fw::string::get(constant(name)), value, "{name}");
        }
    }
}
