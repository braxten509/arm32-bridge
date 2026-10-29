//! Storm8 stand-in checks. The game data here is made up (a tiny catalog in the shape of the
//! recorded login); the real recording is only read by `recorded_traffic_replays_like_the_server`
//! when FARMSTORY_CAPTURE points at it.

use super::*;
use crate::offline::tests::pack_of;
use crate::offline::Pack;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// The recorded login's shape: a new farm with one tree and three ripe strawberries.
fn fixture_login() -> Value {
    json!({
        "gameData": {
            "loginInfo": {"type": "LoginInfo"},
            "news": {"lastReadTime": "x"},
            "userInfo": {"type": "UserInfo", "id": 7, "createdTime": 1000, "cash": 10000, "level": 1, "favorAmount": 10,
                "groupSize": 1, "changeEventVersion": 1, "serverTime": 1000, "avatar": "{\"gender\":\"female\"}"},
            "board": {"type": "Board", "width": 1200, "height": 1200,
                "serializedCells": "0,0,4001,913600,0,0,0;0,120,1001,700,0,0,0;120,0,1001,700,0,0,0;120,120,1001,700,0,0,0",
                "serializedCitizens": "[]", "boardVersion": "1"},
            "storedItems": null,
            "systemDataVersion": "sdv-test",
            "appConstants": {"googleAnalyticsAccountId": "UA-0", "loginTimeout": 14400, "adRefreshSeconds": 300,
                "minimumLevelForItemMastery": 8, "masteryPointsForPlantingItem": 1, "masteryPointsForHarvestingItem": 2,
                "waterHarvestCashBonus": 20, "waterHarvestExperienceBonus": 1, "fertilizeAllItemId": 11054,
                "gatherAllAnimalsTreesItemId": 11056},
            "appResources": {"dialogInsufficientCash": {"dialogMessageDictionary": {"cashPerFavorsBundle": 5000, "favorsPerBundle": 10}}},
            "levelSchedule": [6, 16, 50],
            "itemMastery": {"1001": [{"type": "ItemMastery", "itemId": 1001, "masteryThreshold": 10, "rewardCash": 1000}]},
            "ads": {"menu": {"ads": ["junk"]}},
            "quests": {"9": {"type": "Quest", "questId": "9", "rewardCash": "100"}},
            "categorySlots": {"1": {"type": "CategorySlot", "id": 1}},
            "items": {
                "2": {"type": "Item", "id": 2, "experience": 1, "cost": 10, "category": 100},
                "3": {"type": "Item", "id": 3, "category": 100},
                "1001": {"type": "Item", "id": 1001, "experience": 1, "category": 1, "income": 30, "maturity": 300,
                    "lifespan": 7200, "requirementItemId": 2, "harvestedItemId": 3, "completeFp": 1, "restoreFp": 1},
                "4001": {"type": "Item", "id": 4001, "harvestExperience": 1, "cost": 100, "category": 4, "income": 10,
                    "maturity": 79200, "sellingPrice": 10, "flags": 8},
                "11054": {"type": "Item", "id": 11054, "category": 99, "favorCost": 20, "rewardType": 4}
            },
            "session": {"token": "t"},
            "contentCdnVersion": "1020"
        },
        "callResult": {"success": true, "resetAll": true}
    })
}

struct Fixture {
    pack: Pack,
    state: PathBuf,
    clock: Arc<AtomicI64>,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        Fixture::with_login(name, fixture_login().to_string().into_bytes())
    }

    fn with_login(name: &str, login: Vec<u8>) -> Fixture {
        let dir = std::env::temp_dir().join(format!("gb-storm8-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pack = Pack::parse(pack_of(&[("storm8/login.json", &login)])).unwrap();
        Fixture { pack, state: dir.join("fs.teamlava.com.json"), clock: Arc::new(AtomicI64::new(5000)) }
    }

    /// A server as the game would get after an app start.
    fn server(&self) -> Server {
        let mut server = Server::new(&self.pack, "storm8", self.state.clone()).unwrap();
        let clock = self.clock.clone();
        server.set_clock(Box::new(move || clock.load(Ordering::SeqCst)));
        server
    }
}

fn encode(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.*".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// A call the way StormApiBase sends it: `p` in the body (POST) or the query (GET).
fn call(server: &mut Server, method: &str, p: Value, post: bool) -> Value {
    let p = p.to_string();
    let uri = format!("/api.php?cv=1.9.6.4g&method={method}&ctx=None&nflrt=&st=x&sh=y");
    let request = if post {
        Request { method: "POST".into(), uri, body: format!("p={}", encode(&p)).into_bytes(), host: "fs.teamlava.com".into(), ..Default::default() }
    } else {
        Request { method: "GET".into(), uri: format!("{uri}&p={}", encode(&p)), host: "fs.teamlava.com".into(), ..Default::default() }
    };
    let response = server.answer(&request);
    assert_eq!(response.status, 200);
    serde_json::from_slice(&response.body).expect("answer is JSON")
}

/// applyChanges carrying `events` the way the client piggy-backs them (urlencoded JSON in JSON).
fn apply(server: &mut Server, events: &str, post: bool) -> Value {
    let info = json!({"changeEvents": events}).to_string();
    call(server, "applyChanges", json!({"piggyBackedInfo": encode(&info)}), post)
}

fn login(server: &mut Server, sdv: &str) -> Value {
    call(server, "login", json!({"systemDataVersion": sdv, "clientVersion": "1.9.6.4g"}), true)
}

fn cash(answer: &Value) -> i64 {
    answer["callResult"]["cash"].as_i64().unwrap()
}

#[test]
fn events_change_coins_like_the_recorded_server() {
    let fixture = Fixture::new("replay");
    let mut server = fixture.server();
    assert_eq!(login(&mut server, "0")["gameData"]["userInfo"]["cash"], 10000);
    // One strawberry harvested (+30), then two more (+60); the tutorial steps cost nothing.
    let first = apply(&mut server, "2,5010,user,tutorialState,1,0;3,5011,h,120,0,1001,0;4,5012,user,tutorialState,2,0", true);
    assert_eq!((first["callResult"]["success"].as_bool(), cash(&first), first["callResult"]["favorAmount"].as_i64()), (Some(true), 10030, Some(10)));
    assert_eq!(cash(&apply(&mut server, "5,5020,h,120,120,1001,0;6,5020,user,tutorialState,3,0;7,5021,h,0,120,1001,0", true)), 10090);
    // Plowing a harvested field costs 10 (GET, as the client sends short calls).
    assert_eq!(cash(&apply(&mut server, "8,5100,u,120,120,3,2,0", false)), 10080);
    // Planting strawberries is free; the tree gives 10.
    assert_eq!(cash(&apply(&mut server, "9,5110,u,120,120,2,1001,0;10,5120,h,0,0,4001,0", true)), 10090);
    let user = &login(&mut server, "sdv-test")["gameData"]["userInfo"];
    assert_eq!((user["experience"].as_i64(), user["changeEventVersion"].as_i64(), user["tutorialState"].as_i64()), (Some(3), Some(10), Some(3)));
}

#[test]
fn retried_events_count_once() {
    let fixture = Fixture::new("retry");
    let mut server = fixture.server();
    login(&mut server, "0");
    // The tree stays after a harvest, so only the event version tells a retry from a new harvest.
    let events = "2,5011,h,120,0,1001,0;3,5012,h,0,0,4001,0";
    assert_eq!(cash(&apply(&mut server, events, true)), 10040);
    assert_eq!(cash(&apply(&mut server, events, true)), 10040, "the same events again (an HTTP retry)");
    // A batch that is half old, half new: only the new half counts.
    assert_eq!(cash(&apply(&mut server, "3,5012,h,0,0,4001,0;4,5013,h,0,120,1001,0", true)), 10070);
}

#[test]
fn progress_survives_a_restart() {
    let fixture = Fixture::new("restart");
    let mut server = fixture.server();
    login(&mut server, "0");
    apply(&mut server, "2,5011,h,120,0,1001,0;3,5100,u,120,0,3,2,0;4,5110,u,120,0,2,1001,0;5,5120,st,0,0,4001,0", true);
    drop(server);

    fixture.clock.store(9000, Ordering::SeqCst);
    let mut server = fixture.server();
    let same = login(&mut server, "sdv-test");
    let data = &same["gameData"];
    assert_eq!((data["userInfo"]["cash"].as_i64(), data["userInfo"]["changeEventVersion"].as_i64()), (Some(10020), Some(5)));
    assert_eq!(data["userInfo"]["serverTime"].as_i64(), Some(9000), "a fresh server time each login");
    let cells: Vec<&str> = data["board"]["serializedCells"].as_str().unwrap().split(';').collect();
    assert_eq!(cells, ["0,120,1001,4700,0,0,0", "120,0,1001,5110,0,0,0", "120,120,1001,4700,0,0,0"]);
    assert_eq!(data["storedItems"]["4001"]["count"].as_i64(), Some(1));
    assert_eq!(data["storedItems"]["4001"]["type"], "UserItem");
    assert!(data.get("items").is_none() && data.get("categorySlots").is_none(), "cached catalog left out like the real server");
    assert_eq!(same["callResult"]["success"], true);

    let fresh_client = &login(&mut server, "0")["gameData"];
    assert_eq!(fresh_client["items"]["1001"]["income"], 30, "a client without the catalog gets it");
    assert_eq!(fresh_client["userInfo"]["cash"], 10020);
}

#[test]
fn a_new_player_starts_from_the_recorded_farm() {
    let fixture = Fixture::new("new");
    let mut server = fixture.server();
    let answer = login(&mut server, "0");
    let data = &answer["gameData"];
    let user = &data["userInfo"];
    assert_eq!((user["cash"].as_i64(), user["favorAmount"].as_i64(), user["changeEventVersion"].as_i64()), (Some(10000), Some(10), Some(1)));
    assert_eq!((user["serverTime"].as_i64(), user["type"].as_str()), (Some(5000), Some("UserInfo")));
    assert_eq!(user.as_object().unwrap().keys().next().map(String::as_str), Some("type"), "Storm8's parser wants type first");
    // The recorded farm, its timers moved to now (the recording was made at 1000).
    assert_eq!(data["board"]["serializedCells"], "0,0,4001,917600,0,0,0;0,120,1001,4700,0,0,0;120,0,1001,4700,0,0,0;120,120,1001,4700,0,0,0");
    assert_eq!((data["board"]["width"].as_i64(), data["board"]["type"].as_str()), (Some(1200), Some("Board")));
    assert_eq!(data["systemDataVersion"], "sdv-test");
    assert_eq!(data["contentCdnVersion"], "1020");
    assert_eq!(data["ads"], json!({}));
    assert_eq!((data["appConstants"]["loginTimeout"].as_i64(), data["appConstants"]["adRefreshSeconds"].as_i64()), (Some(0), Some(0)));
    assert_eq!(data["items"]["4001"]["sellingPrice"], 10);
    assert_eq!(answer["callResult"]["success"], true);
}

/// The real recording replayed: the second login's coins, XP and farm must come out the same.
/// Runs only with FARMSTORY_CAPTURE=<…/farmstory/capture>.
#[test]
fn recorded_traffic_replays_like_the_server() {
    let Some(capture) = std::env::var_os("FARMSTORY_CAPTURE").map(PathBuf::from) else {
        eprintln!("FARMSTORY_CAPTURE not set: skipped");
        return;
    };
    let body = |file: &str| {
        let text = std::fs::read_to_string(capture.join(file)).unwrap();
        let mut lines = text.splitn(3, '\n');
        let first = lines.next().unwrap().to_string();
        (first, lines.nth(1).unwrap_or("").to_string())
    };
    // The pack's trimmed copy when it sits next to the capture (what goes on the phone).
    let shipped = std::fs::read(capture.join("../offline-pack/storm8/login.json")).ok();
    let fixture = Fixture::with_login("capture", shipped.unwrap_or_else(|| body("fs1/004-resp.txt").1.into_bytes()));
    let recorded_login: Value = serde_json::from_str(&body("fs1/004-resp.txt").1).unwrap();
    fixture.clock.store(recorded_login["gameData"]["userInfo"]["serverTime"].as_i64().unwrap(), Ordering::SeqCst);
    let mut server = fixture.server();
    let first = login(&mut server, "0");
    assert!(first["gameData"]["items"].as_object().map_or(0, |items| items.len()) > 1000, "the whole catalog for a new game");
    for file in ["fs1/002-req.txt", "fs1/003-req.txt", "fs2/001-req.txt", "fs2/007-req.txt", "fs2/008-req.txt"] {
        let (line, form) = body(file);
        let mut words = line.split(' ');
        let (method, uri) = (words.next().unwrap().to_string(), words.next().unwrap().to_string());
        let answer = server.answer(&Request { method, uri, body: form.trim_end().as_bytes().to_vec(), host: "fs.teamlava.com".into(), ..Default::default() });
        let answer: Value = serde_json::from_slice(&answer.body).unwrap();
        assert_eq!(answer["callResult"]["success"], true, "{file}");
    }
    let second: Value = serde_json::from_str(&body("fs2/030-resp.txt").1).unwrap();
    fixture.clock.store(second["gameData"]["userInfo"]["serverTime"].as_i64().unwrap(), Ordering::SeqCst);
    let ours = login(&mut server, second["gameData"]["systemDataVersion"].as_str().unwrap());
    for key in ["cash", "favorAmount", "experience", "changeEventVersion", "groupSize"] {
        assert_eq!(ours["gameData"]["userInfo"][key], second["gameData"]["userInfo"][key], "userInfo.{key}");
    }
    assert_eq!(ours["gameData"]["board"]["serializedCells"], second["gameData"]["board"]["serializedCells"]);
}
