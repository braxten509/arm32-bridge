//! The Storm8/TeamLava game server, answered on the phone (docs/offline-plan.md section 4;
//! the protocol is in ~/dev/arm32-bridge-games/farmstory/PROTOCOL.md).
//!
//! The pack folder holds `login.json`: a recorded first login (new player, full catalog).
//! Every answer is built from it and from the player's farm (farm.rs), which is saved as JSON
//! after each change. Calls look like `GET|POST /api.php?method=<m>&...` with one JSON object
//! in the parameter `p` (query or form body); every blocking call may carry the game's change
//! events in `p.piggyBackedInfo`, and those are applied before the call itself.

mod farm;
#[cfg(test)]
mod tests;

use super::{percent_decode, Pack, Request, Response};
use farm::{num, Catalog, Farm};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub struct Server {
    catalog: Catalog,
    /// The recorded login's gameData without the catalog parts below.
    login: Map<String, Value>,
    /// Sent only to a game that hasn't cached this systemDataVersion yet, like the real
    /// server: the item catalog (as JSON text, ~1 MB), categorySlots and quests.
    items: String,
    category_slots: Value,
    quests: Value,
    login_result: Value,
    system_data_version: String,
    /// Loaded at the first call, so a new farm's timers start then.
    farm: Option<Farm>,
    state: PathBuf,
    clock: Box<dyn Fn() -> i64 + Send>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// A dialog the game shows after a purchase (DialogManager.showDialog).
fn dialog(success: bool, text: &str) -> Value {
    let (header, title) = if success { ("header_congrats.png", "SUCCESS!") } else { ("header_failure.png", "OH DARN!") };
    json!({"dialogType": "MessageDialogView", "headerImage": header, "title": title, "description": text})
}

/// Calls the game makes without any farm change to keep; they succeed with the usual totals.
const QUIET_METHODS: &[&str] = &[
    "applyChanges", "setDeviceToken", "closeHotTip", "trackRefId", "setAccountName", "setUserName", "postComment",
    "deleteComment", "acceptGroupMember", "rejectGroupMember", "removeGroupMember", "acceptAllGroupInvites",
    "inviteWithAccountName", "sendGifts", "sendSuggestedGifts", "acceptGift", "ignoreGift", "requestPart", "sendPart",
    "ignorePartRequest", "facebookLogout", "forceUpdateData", "resetUser", "setAvatar",
];

/// Paid with real money or through a store: no offline answer, the game says it failed.
const STORE_METHODS: &[&str] = &["getOffers", "getAarkiOffers", "confirmGetJar", "processAndroidMarketTransactions", "processAmazonMarketTransaction"];

impl Server {
    pub fn new(pack: &Pack, folder: &str, state: PathBuf) -> Result<Server, String> {
        let name = format!("{}/login.json", folder.trim_end_matches('/'));
        let bytes = pack.file(&name).ok_or(format!("the pack has no {name}"))?;
        let mut recorded: Value = serde_json::from_slice(bytes).map_err(|e| format!("{name}: {e}"))?;
        let catalog = Catalog::read(&recorded["gameData"]);
        let login_result = recorded["callResult"].take();
        let Value::Object(mut login) = recorded["gameData"].take() else { return Err(format!("{name} has no gameData")) };
        let items = serde_json::to_string(&login.remove("items").unwrap_or(json!({}))).map_err(|e| e.to_string())?;
        let category_slots = login.remove("categorySlots").unwrap_or(json!({}));
        let quests = login.remove("quests").unwrap_or(json!({}));
        let system_data_version = login.get("systemDataVersion").and_then(Value::as_str).unwrap_or("offline").to_string();
        // Nothing to fetch offline: no ads, no news, and no timers that log in again or
        // ask for ads (GameControllerBase: 0 turns them off).
        login.insert("ads".into(), json!({}));
        login.insert("availableGameApps".into(), json!([]));
        login.insert("news".into(), json!({}));
        if let Some(Value::Object(constants)) = login.get_mut("appConstants") {
            constants.insert("loginTimeout".into(), json!(0));
            constants.insert("adRefreshSeconds".into(), json!(0));
        }
        login.insert("contentCdnVersion".into(), json!("1020"));
        Ok(Server { catalog, login, items, category_slots, quests, login_result, system_data_version, farm: None, state, clock: Box::new(unix_now) })
    }

    #[cfg(test)]
    fn set_clock(&mut self, clock: Box<dyn Fn() -> i64 + Send>) {
        self.clock = clock;
    }

    /// The saved farm, or the recorded new player's.
    fn load(&mut self, now: i64) -> Farm {
        let saved = std::fs::read(&self.state).ok().map(|bytes| serde_json::from_slice::<Value>(&bytes));
        match saved {
            Some(Ok(saved)) if saved["userInfo"].is_object() => Farm::read(&saved),
            other => {
                if let Some(broken) = other {
                    // Keep a broken save for a look later instead of overwriting it.
                    let aside = self.state.with_extension(format!("broken-{now}.json"));
                    crate::log::line(&format!("[offline] storm8: unreadable save ({}), kept as {}", broken.err().map_or("no userInfo".into(), |e| e.to_string()), aside.display()));
                    let _ = std::fs::rename(&self.state, aside);
                }
                Farm::new_player(&Value::Object(self.login.clone()), now)
            }
        }
    }

    fn save(&self, farm: &Farm, now: i64) {
        let board = self.login.get("board").cloned().unwrap_or(Value::Null);
        let text = match serde_json::to_vec(&farm.saved(&self.catalog, &board, now)) {
            Ok(text) => text,
            Err(e) => return crate::log::line(&format!("[offline] storm8: can't save: {e}")),
        };
        if let Some(parent) = self.state.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.state.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &self.state)) {
            crate::log::line(&format!("[offline] storm8: can't save {}: {e}", self.state.display()));
        }
    }

    pub fn answer(&mut self, request: &Request) -> Response {
        let now = (self.clock)();
        let mut farm = match self.farm.take() {
            Some(farm) => farm,
            None => self.load(now),
        };
        let query = request.query();
        let method = query.get("method").cloned().unwrap_or_default();
        let p = query.get("p").cloned().or_else(|| request.form().remove("p")).unwrap_or_default();
        let p: Value = serde_json::from_str(&p).unwrap_or(json!({}));
        let before = (farm.version, farm.cash, farm.favor, farm.width, farm.stored.clone(), farm.user.clone());
        if let Some(events) = p["piggyBackedInfo"].as_str().and_then(|info| serde_json::from_str::<Value>(&percent_decode(info)).ok()) {
            farm.apply_events(&self.catalog, events["changeEvents"].as_str().unwrap_or(""));
        }
        let body = self.call(&mut farm, &method, &p, now);
        if method == "login" || before != (farm.version, farm.cash, farm.favor, farm.width, farm.stored.clone(), farm.user.clone()) {
            self.save(&farm, now);
        }
        self.farm = Some(farm);
        Response::json(body)
    }

    fn call(&self, farm: &mut Farm, method: &str, p: &Value, now: i64) -> String {
        match method {
            "login" => return self.login(farm, p, now),
            "getAds" => json!({"callResult": {"success": true, "clearLocalData": false}}),
            // Offline there are no neighbours to visit; the game says the farm didn't load.
            "getBoardForProfileId" => json!({"callResult": {"success": false}}),
            "buyItem" => self.buy_item(farm, num(&p["itemId"]), num(&p["count"]).max(1), now),
            "buyCash" => self.buy_cash(farm, num(&p["fp"]), now),
            "buyCrate" => json!({"callResult": {"success": true, "dialogMessageDictionary": dialog(false, "Crates can't be opened offline.")}}),
            "getStoredItems" => json!({"gameData": {"storedItems": farm.stored_items()}, "callResult": {"success": true}}),
            "getNews2" => json!({"gameData": {"news": {}}, "callResult": {"success": true}}),
            "getGroupData" => {
                let recorded = |key: &str| self.login.get(key).cloned().unwrap_or(json!([]));
                json!({"gameData": {"groupMembers": recorded("groupMembers"), "groupInvites": [], "groupMarker": recorded("groupMarker")}, "callResult": {"success": true}})
            }
            "getKarmaList" => json!({"gameData": {"karmaList": []}, "callResult": {"success": true}}),
            "getComments" => json!({"callResult": {"success": true, "comments": []}}),
            "getInstalledAppleBinaryList" => json!({"callResult": {"success": true, "ids": []}}),
            "setUserAvatar" => {
                if let Some(avatar) = p["avatar"].as_str() {
                    farm.user.insert("avatar".into(), json!(avatar));
                }
                self.totals(farm)
            }
            _ if STORE_METHODS.contains(&method) => json!({"callResult": {"success": false}}),
            _ if QUIET_METHODS.contains(&method) => self.totals(farm),
            _ => {
                crate::log::line(&format!("[offline] storm8: unknown method {method}"));
                json!({"callResult": {"success": true}})
            }
        }
        .to_string()
    }

    /// The applyChanges answer: the game takes over these coins, gems and neighbour count.
    fn totals(&self, farm: &Farm) -> Value {
        let mut data = Map::new();
        for key in ["session", "contentCdnVersion", "cdnVersion"] {
            if let Some(value) = self.login.get(key) {
                data.insert(key.into(), value.clone());
            }
        }
        data.insert("mandatoryNotices".into(), json!([]));
        data.insert("optionalNotices".into(), json!([]));
        let group_size = farm.user.get("groupSize").map_or(1, num);
        json!({"gameData": data, "callResult": {"success": true, "waterMask": "", "favorAmount": farm.favor, "cash": farm.cash, "groupSize": group_size, "clearLocalData": false}})
    }

    fn login(&self, farm: &Farm, p: &Value, now: i64) -> String {
        let mut data = self.login.clone();
        data.insert("userInfo".into(), farm.user_info(&self.catalog, now));
        let board = farm.board(data.get("board").unwrap_or(&Value::Null));
        data.insert("board".into(), board);
        data.insert("storedItems".into(), farm.stored_items());
        data.insert("userItemMastery".into(), farm.item_mastery());
        data.insert("userEnsembleMastery".into(), farm.ensemble_mastery_value());
        let cached = p["systemDataVersion"].as_str() == Some(self.system_data_version.as_str());
        if !cached {
            data.insert("categorySlots".into(), self.category_slots.clone());
            data.insert("quests".into(), self.quests.clone());
        }
        let mut text = Value::Object(data).to_string();
        if !cached {
            // The catalog goes in as the recorded text (re-reading 1 MB for every login is waste).
            text.pop();
            text.push_str(",\"items\":");
            text.push_str(&self.items);
            text.push('}');
        }
        format!("{{\"gameData\":{text},\"callResult\":{}}}", self.login_result)
    }

    /// An answer that also hands the game its new coins, gems, farm and storage.
    fn bought(&self, farm: &Farm, text: &str, board: bool, now: i64) -> Value {
        let mut data = Map::new();
        data.insert("userInfo".into(), farm.user_info(&self.catalog, now));
        data.insert("storedItems".into(), farm.stored_items());
        if board {
            data.insert("board".into(), farm.board(self.login.get("board").unwrap_or(&Value::Null)));
        }
        json!({"gameData": data, "callResult": {"success": true, "dialogMessageDictionary": dialog(true, text)}})
    }

    fn refused(text: &str) -> Value {
        json!({"callResult": {"success": true, "dialogMessageDictionary": dialog(false, text)}})
    }

    /// buyItem: market items without a place on the farm (MarketItemView: width 0): farm
    /// expansions (rewardType 1), coin packs (2) and building parts.
    fn buy_item(&self, farm: &mut Farm, id: i64, count: i64, now: i64) -> Value {
        let Some(item) = self.catalog.item(id).cloned() else { return Server::refused("This isn't available offline.") };
        let (cost, favor_cost) = (item.cost * count, item.favor_cost * count);
        if farm.cash < cost {
            return Server::refused("You don't have enough coins.");
        }
        if farm.favor < favor_cost {
            return Server::refused("You don't have enough Gems.");
        }
        match item.reward_type {
            1 => {
                if item.reward_value <= farm.width && item.reward_value2 <= farm.height {
                    return Server::refused("Your farm is already this big.");
                }
                if self.catalog.level(farm.experience) < item.min_level {
                    return Server::refused(&format!("You need level {} for this.", item.min_level));
                }
                farm.cash -= cost;
                farm.favor -= favor_cost;
                farm.width = farm.width.max(item.reward_value);
                farm.height = farm.height.max(item.reward_value2);
                self.bought(farm, &format!("Your farm is now {}x{}!", farm.width / 120, farm.height / 120), true, now)
            }
            2 => {
                farm.cash += item.reward_value * count - cost;
                farm.favor -= favor_cost;
                self.bought(farm, &format!("You got {} coins!", item.reward_value * count), false, now)
            }
            _ if item.width == 0 && item.storable() => {
                farm.cash -= cost;
                farm.favor -= favor_cost;
                farm.store(&self.catalog, id, count, now);
                self.bought(farm, &format!("{} is in your storage.", item.name), false, now)
            }
            _ => Server::refused("This isn't available offline."),
        }
    }

    /// buyCash: gems for coins, at the rate of the game's "not enough coins" offer.
    fn buy_cash(&self, farm: &mut Farm, gems: i64, now: i64) -> Value {
        if gems <= 0 || self.catalog.favors_per_bundle <= 0 {
            return Server::refused("This isn't available offline.");
        }
        if farm.favor < gems {
            return Server::refused("You don't have enough Gems.");
        }
        let coins = gems / self.catalog.favors_per_bundle * self.catalog.cash_per_bundle;
        farm.favor -= gems;
        farm.cash += coins;
        self.bought(farm, &format!("You got {coins} coins!"), false, now)
    }
}
