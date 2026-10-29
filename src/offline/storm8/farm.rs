//! The player's farm and the rules that change it. The game works out coins, gems and XP
//! itself, then reports what the player did as change events; like the real server, the
//! stand-in replays each event on the saved farm, because after every applyChanges the game
//! takes over the server's coins and gems. The rules mirror the game's own code
//! (BoardManagerBase, UserInfo.gainMastery, QuestManager, EnsembleRowView; see
//! ~/dev/arm32-bridge-games/farmstory/PROTOCOL.md section 3).

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// A number the way Storm8 writes them: 12, 12.5 or "12"; anything else 0.
pub fn num(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Value::String(s) => s.trim().parse::<i64>().ok().or_else(|| s.trim().parse::<f64>().ok().map(|f| f as i64)).unwrap_or(0),
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

/// What the rules need of one catalog item (model/ItemBase.java).
#[derive(Debug, Default, Clone)]
pub struct Item {
    pub id: i64,
    pub name: String,
    pub cost: i64,
    pub favor_cost: i64,
    pub income: i64,
    pub experience: i64,
    pub harvest_experience: i64,
    pub maturity: i64,
    pub lifespan: i64,
    pub selling_price: i64,
    pub harvested_item_id: i64,
    pub category: i64,
    pub flags: i64,
    pub group: i64,
    pub subgroup: i64,
    pub reward_type: i64,
    pub reward_value: i64,
    pub reward_value2: i64,
    pub width: i64,
    pub min_level: i64,
    pub complete_fp: i64,
    pub restore_fp: i64,
    pub unlock_fp: i64,
    /// Constructable buildings: part item -> how many.
    pub parts: Vec<(i64, i64)>,
}

impl Item {
    fn read(value: &Value) -> Item {
        let n = |key: &str| num(&value[key]);
        let parts = value["parts"].as_object().map_or_else(Vec::new, |parts| parts.iter().map(|(id, part)| (id.parse().unwrap_or(0), num(&part["quantity"]))).collect());
        Item {
            id: n("id"),
            name: value["name"].as_str().unwrap_or("").to_string(),
            cost: n("cost"),
            favor_cost: n("favorCost"),
            income: n("income"),
            experience: n("experience"),
            harvest_experience: n("harvestExperience"),
            maturity: n("maturity"),
            lifespan: n("lifespan"),
            selling_price: n("sellingPrice"),
            harvested_item_id: n("harvestedItemId"),
            category: n("category"),
            flags: n("flags"),
            group: n("group"),
            subgroup: n("subgroup"),
            reward_type: n("rewardType"),
            reward_value: n("rewardValue"),
            reward_value2: n("rewardValue2"),
            width: n("width"),
            min_level: n("minLevel"),
            complete_fp: n("completeFp"),
            restore_fp: n("restoreFp"),
            unlock_fp: n("unlockFp"),
            parts,
        }
    }

    /// The id storage counts it under (ItemBase.getUnrotatedItemId).
    pub fn unrotated(&self) -> i64 {
        if self.subgroup > 0 {
            self.subgroup
        } else if self.group > 0 {
            self.group
        } else {
            self.id
        }
    }

    pub fn storable(&self) -> bool {
        self.flags & 8 != 0
    }
}

/// A reward: a mastery rank, a quest, a collection.
#[derive(Debug, Default, Clone)]
pub struct Reward {
    pub threshold: i64,
    pub cash: i64,
    pub experience: i64,
    pub favor: i64,
    pub item: i64,
    pub item_count: i64,
    pub recurring_percent: i64,
}

impl Reward {
    fn read(value: &Value) -> Reward {
        let n = |key: &str| num(&value[key]);
        Reward {
            threshold: n("masteryThreshold"),
            cash: n("rewardCash"),
            experience: n("rewardExperience"),
            favor: n("rewardFavorPoints") + n("rewardFP") + n("rewardFp"),
            item: n("rewardItemId"),
            item_count: n("rewardItemQuantity").max(1),
            recurring_percent: n("rewardRecurringPercent"),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Ensemble {
    items: Vec<(i64, i64)>,
    reward: Reward,
    masteries: Vec<Reward>,
}

/// The game's rules data, read from the recorded login.
#[derive(Debug, Default)]
pub struct Catalog {
    pub items: HashMap<i64, Item>,
    mastery: HashMap<i64, Vec<Reward>>,
    levels: Vec<i64>,
    quests: HashMap<i64, Reward>,
    ensembles: HashMap<i64, Ensemble>,
    mastery_min_level: i64,
    plant_points: i64,
    harvest_points: i64,
    water_cash: i64,
    water_experience: i64,
    fertilize_all_item: i64,
    gather_all_item: i64,
    /// buyCash: coins per bundle of gems (appResources.dialogInsufficientCash).
    pub cash_per_bundle: i64,
    pub favors_per_bundle: i64,
}

impl Catalog {
    pub fn read(data: &Value) -> Catalog {
        let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
        let items = data["items"].as_object().map_or_else(HashMap::new, |items| items.values().map(Item::read).map(|item| (item.id, item)).collect());
        let mastery = data["itemMastery"].as_object().map_or_else(HashMap::new, |all| all.iter().map(|(id, ranks)| (id.parse().unwrap_or(0), list(ranks).iter().map(Reward::read).collect())).collect());
        let quests = data["quests"].as_object().map_or_else(HashMap::new, |all| all.iter().map(|(id, quest)| (id.parse().unwrap_or(0), Reward::read(quest))).collect());
        let ensembles = list(&data["ensembles"]).iter().map(|ensemble| {
            let quantities = list(&ensemble["quantities"]);
            let items = list(&ensemble["items"]).iter().enumerate().map(|(i, id)| (num(id), quantities.get(i).map_or(1, num))).collect();
            let mut reward = Reward::read(ensemble);
            reward.item_count = 1;
            (num(&ensemble["id"]), Ensemble { items, reward, masteries: list(&ensemble["masteries"]).iter().map(Reward::read).collect() })
        }).collect();
        let constants = &data["appConstants"];
        let n = |key: &str| num(&constants[key]);
        let bundle = &data["appResources"]["dialogInsufficientCash"]["dialogMessageDictionary"];
        Catalog {
            items,
            mastery,
            levels: list(&data["levelSchedule"]).iter().map(num).collect(),
            quests,
            ensembles,
            mastery_min_level: n("minimumLevelForItemMastery"),
            plant_points: n("masteryPointsForPlantingItem"),
            harvest_points: n("masteryPointsForHarvestingItem"),
            water_cash: n("waterHarvestCashBonus"),
            water_experience: n("waterHarvestExperienceBonus"),
            fertilize_all_item: n("fertilizeAllItemId"),
            gather_all_item: n("gatherAllAnimalsTreesItemId"),
            cash_per_bundle: num(&bundle["cashPerFavorsBundle"]),
            favors_per_bundle: num(&bundle["favorsPerBundle"]),
        }
    }

    pub fn item(&self, id: i64) -> Option<&Item> {
        self.items.get(&id)
    }

    /// The player's level for an XP total (UserInfo.getLevel).
    pub fn level(&self, experience: i64) -> i64 {
        self.levels.iter().position(|&needed| experience < needed).map_or(self.levels.len() as i64 + 1, |at| at as i64 + 1)
    }
}

/// One thing on the farm: "x,z,itemId,startTime,flags,secondaryItemId,state" (BoardBase).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Cell {
    pub x: i64,
    pub z: i64,
    pub item: i64,
    pub start: i64,
    pub flags: i64,
    pub secondary: i64,
    pub state: i64,
}

const WATERED: i64 = 1;
const FERTILIZED: i64 = 2;
const UNDER_CONSTRUCTION: i64 = 256;

impl Cell {
    fn parse(text: &str) -> Option<Cell> {
        let f: Vec<i64> = text.split(',').map(|word| word.trim().parse().unwrap_or(0)).collect();
        if f.len() < 3 {
            return None;
        }
        let at = |i: usize| f.get(i).copied().unwrap_or(0);
        Some(Cell { x: at(0), z: at(1), item: at(2), start: at(3), flags: at(4), secondary: at(5), state: at(6) })
    }

    fn text(&self) -> String {
        format!("{},{},{},{},{},{},{}", self.x, self.z, self.item, self.start, self.flags, self.secondary, self.state)
    }
}

/// The player's progress: what the stand-in saves and answers logins with.
#[derive(Debug, Clone)]
pub struct Farm {
    /// userInfo as the game knows it (settings, avatar, tutorial, ...), key order kept.
    pub user: Map<String, Value>,
    pub cash: i64,
    pub favor: i64,
    pub experience: i64,
    /// The last change event applied (userInfo.changeEventVersion).
    pub version: i64,
    pub width: i64,
    pub height: i64,
    pub cells: Vec<Cell>,
    /// Storage: unrotated item id -> (count, lastStoredTime).
    pub stored: BTreeMap<i64, (i64, i64)>,
    /// Item mastery: item id -> (level, progress).
    pub mastery: BTreeMap<i64, (i64, i64)>,
    pub ensemble_mastery: BTreeMap<i64, (i64, i64)>,
}

fn pairs(value: &Value, first: &str, second: &str) -> BTreeMap<i64, (i64, i64)> {
    value.as_object().map_or_else(BTreeMap::new, |all| all.iter().map(|(id, entry)| (id.parse().unwrap_or(0), (num(&entry[first]), num(&entry[second])))).collect())
}

impl Farm {
    /// A farm from login-shaped data: the recorded new-player login or a saved farm.
    pub fn read(data: &Value) -> Farm {
        let user = data["userInfo"].as_object().cloned().unwrap_or_default();
        let board = &data["board"];
        let n = |key: &str| num(&user.get(key).cloned().unwrap_or(Value::Null));
        Farm {
            cash: n("cash"),
            favor: n("favorAmount"),
            experience: n("experience"),
            version: n("changeEventVersion"),
            width: num(&board["width"]),
            height: num(&board["height"]),
            cells: board["serializedCells"].as_str().unwrap_or("").split(';').filter_map(Cell::parse).collect(),
            stored: pairs(&data["storedItems"], "count", "lastStoredTime"),
            mastery: pairs(&data["userItemMastery"], "masteryLevel", "masteryProgress"),
            ensemble_mastery: pairs(&data["userEnsembleMastery"], "masteryLevel", "masteryProgress"),
            user,
        }
    }

    /// The recorded first farm, its timers moved from the recording's time to `now` (the
    /// crops were ripe and the tree growing at the recorded login).
    pub fn new_player(data: &Value, now: i64) -> Farm {
        let mut farm = Farm::read(data);
        let shift = now - num(&data["userInfo"]["serverTime"]);
        for cell in farm.cells.iter_mut().filter(|cell| cell.start > 0) {
            cell.start += shift;
        }
        for key in ["createdTime", "previousUserLastLoginTime"] {
            if farm.user.contains_key(key) {
                farm.user.insert(key.into(), json!(now));
            }
        }
        farm
    }

    /// userInfo for an answer (serverTime = now; the game measures time from it).
    pub fn user_info(&self, catalog: &Catalog, now: i64) -> Value {
        let mut user = self.user.clone();
        for (key, value) in [("cash", self.cash), ("level", catalog.level(self.experience)), ("favorAmount", self.favor), ("experience", self.experience), ("changeEventVersion", self.version), ("serverTime", now)] {
            user.insert(key.into(), json!(value));
        }
        user.remove("clientTime");
        Value::Object(user)
    }

    /// The board for an answer, on top of the recorded board's other keys.
    pub fn board(&self, template: &Value) -> Value {
        let mut board = template.as_object().cloned().unwrap_or_default();
        board.insert("width".into(), json!(self.width));
        board.insert("height".into(), json!(self.height));
        board.insert("serializedCells".into(), json!(self.cells.iter().map(Cell::text).collect::<Vec<_>>().join(";")));
        board.remove("cells");
        Value::Object(board)
    }

    pub fn stored_items(&self) -> Value {
        if self.stored.is_empty() {
            return Value::Null;
        }
        Value::Object(self.stored.iter().map(|(&id, &(count, last))| (id.to_string(), json!({"type": "UserItem", "itemId": id, "count": count, "lastStoredTime": last}))).collect())
    }

    pub fn item_mastery(&self) -> Value {
        if self.mastery.is_empty() {
            return Value::Null;
        }
        Value::Object(self.mastery.iter().map(|(&id, &(level, progress))| (id.to_string(), json!({"type": "UserItemMastery", "itemId": id, "masteryLevel": level, "masteryProgress": progress}))).collect())
    }

    pub fn ensemble_mastery_value(&self) -> Value {
        if self.ensemble_mastery.is_empty() {
            return Value::Null;
        }
        Value::Object(self.ensemble_mastery.iter().map(|(&id, &(level, progress))| (id.to_string(), json!({"type": "UserEnsembleMastery", "ensembleId": id, "masteryLevel": level, "masteryProgress": progress}))).collect())
    }

    /// What the save file holds: the login's shapes.
    pub fn saved(&self, catalog: &Catalog, board_template: &Value, now: i64) -> Value {
        json!({
            "userInfo": self.user_info(catalog, now),
            "board": self.board(board_template),
            "storedItems": self.stored_items(),
            "userItemMastery": self.item_mastery(),
            "userEnsembleMastery": self.ensemble_mastery_value(),
        })
    }

    fn level(&self, catalog: &Catalog) -> i64 {
        catalog.level(self.experience)
    }

    pub fn store(&mut self, catalog: &Catalog, id: i64, count: i64, now: i64) {
        let Some(item) = catalog.item(id) else { return };
        if !item.storable() || count <= 0 {
            return;
        }
        let entry = self.stored.entry(item.unrotated()).or_insert((0, now));
        entry.0 += count;
        entry.1 = now;
    }

    fn unstore(&mut self, key: i64, count: i64) -> bool {
        match self.stored.get_mut(&key) {
            Some(entry) if entry.0 >= count => {
                entry.0 -= count;
                if entry.0 == 0 {
                    self.stored.remove(&key);
                }
                true
            }
            _ => false,
        }
    }

    fn stored_count(&self, key: i64) -> i64 {
        self.stored.get(&key).map_or(0, |entry| entry.0)
    }

    fn reward(&mut self, catalog: &Catalog, reward: &Reward, now: i64) {
        self.cash += reward.cash;
        self.experience += reward.experience;
        self.favor += reward.favor;
        if reward.item > 0 {
            self.store(catalog, reward.item, reward.item_count.max(1), now);
        }
    }

    /// UserInfo.gainMastery: from the mastery level on, planting and harvesting fill an item's
    /// mastery; each full rank pays its reward.
    fn gain_mastery(&mut self, catalog: &Catalog, id: i64, points: i64, now: i64) {
        let Some(ranks) = catalog.mastery.get(&id) else { return };
        if self.level(catalog) < catalog.mastery_min_level {
            return;
        }
        let entry = self.mastery.entry(id).or_insert((0, 0));
        entry.1 += points;
        let (level, progress) = *entry;
        if let Some(rank) = ranks.get(level as usize).filter(|rank| progress >= rank.threshold) {
            *entry = (level + 1, 0);
            let rank = rank.clone();
            self.reward(catalog, &rank, now);
        }
    }

    /// UserItemMastery.incomeMultiplier.
    fn income(&self, catalog: &Catalog, item: &Item) -> i64 {
        let percent = self.mastery.get(&item.id).and_then(|&(level, _)| catalog.mastery.get(&item.id)?.get((level - 1).clamp(0, 3) as usize)).map_or(0, |rank| rank.recurring_percent);
        (item.income as f32 * (1.0 + percent as f32 / 100.0)) as i64
    }

    fn dead(&self, catalog: &Catalog, cell: &Cell, now: i64) -> bool {
        let lifespan = catalog.item(cell.item).map_or(0, |item| item.lifespan);
        self.level(catalog) > 1 && cell.flags & FERTILIZED == 0 && lifespan > 0 && cell.start > 0 && now >= cell.start + lifespan
    }

    fn ripe(&self, catalog: &Catalog, cell: &Cell, now: i64) -> bool {
        let maturity = catalog.item(cell.item).map_or(0, |item| item.maturity);
        maturity > 0 && cell.start > 0 && (cell.flags & FERTILIZED != 0 || (!self.dead(catalog, cell, now) && now >= cell.start + maturity))
    }

    /// CellBase.getFertilizeCost (Farm Story's costs aren't prorated).
    fn fertilize_cost(&self, catalog: &Catalog, cell: &Cell, now: i64) -> i64 {
        let id = if cell.secondary > 0 { cell.secondary } else { cell.item };
        let Some(item) = catalog.item(id) else { return 0 };
        if self.dead(catalog, cell, now) {
            item.restore_fp
        } else {
            item.complete_fp
        }
    }

    fn can_fertilize(&self, catalog: &Catalog, cell: &Cell, now: i64) -> bool {
        let maturity = catalog.item(cell.item).map_or(0, |item| item.maturity);
        maturity > 0 && cell.flags & FERTILIZED == 0 && (self.dead(catalog, cell, now) || !self.ripe(catalog, cell, now))
    }

    fn find(&self, x: i64, z: i64, item: i64) -> Option<usize> {
        self.cells.iter().position(|cell| cell.x == x && cell.z == z && cell.item == item)
    }

    /// BoardManagerBase.harvestCell. The game only sends a harvest it allowed, so a single
    /// harvest pays without a ripeness check (the game's clock and ours agree only roughly).
    fn harvest(&mut self, catalog: &Catalog, at: usize, now: i64) {
        let Some(item) = catalog.item(self.cells[at].item).cloned() else { return };
        let watered = self.cells[at].flags & WATERED != 0;
        self.cash += self.income(catalog, &item);
        self.gain_mastery(catalog, item.id, catalog.harvest_points, now);
        self.experience += item.harvest_experience;
        if watered {
            self.cash += catalog.water_cash;
            self.experience += catalog.water_experience;
        }
        let cell = &mut self.cells[at];
        cell.flags &= 1024;
        if item.harvested_item_id > 0 {
            cell.item = item.harvested_item_id;
            // The recorded server leaves a harvested field without a timer.
            cell.start = if catalog.item(item.harvested_item_id).is_some_and(|next| next.maturity > 0) { now } else { 0 };
        } else {
            cell.start = if item.maturity > 0 { now } else { 0 };
        }
    }

    /// Applies "version,time,code,fields...,collectionItemId;..." (GameContextBase
    /// .getChangeEvents). Events at or below the saved version were applied already (a retried
    /// call) and are skipped. Returns how many were applied.
    pub fn apply_events(&mut self, catalog: &Catalog, text: &str) -> usize {
        let mut applied = 0;
        for event in text.split(';').map(str::trim).filter(|event| !event.is_empty()) {
            let fields: Vec<&str> = event.split(',').collect();
            let (Some(version), Some(time)) = (fields.first().and_then(|v| v.parse::<i64>().ok()), fields.get(1).and_then(|t| t.parse::<i64>().ok())) else {
                crate::log::line(&format!("[offline] storm8: unreadable change event {event:?}"));
                continue;
            };
            if version <= self.version || fields.len() < 3 {
                continue;
            }
            // Every event ends with the collection item the game dropped for it (0 = none).
            let (args, collection) = match fields.len() {
                3 => (&fields[3..], 0),
                n => (&fields[3..n - 1], fields[n - 1].parse().unwrap_or(0)),
            };
            self.apply(catalog, fields[2], args, time);
            if collection > 0 {
                self.store(catalog, collection, 1, time);
            }
            self.version = version;
            applied += 1;
        }
        applied
    }

    fn apply(&mut self, catalog: &Catalog, code: &str, args: &[&str], now: i64) {
        let n = |i: usize| args.get(i).and_then(|word| word.trim().parse::<i64>().ok()).unwrap_or(0);
        let item = |id: i64| catalog.item(id).cloned();
        match code {
            // Place a new item (BoardManagerBase.addCell).
            "a" => {
                let Some(new) = item(n(2)) else { return self.unknown_item(code, n(2)) };
                self.cash -= new.cost;
                self.favor -= new.favor_cost;
                let constructable = !new.parts.is_empty();
                self.experience += if constructable { 0 } else { new.experience };
                let (start, flags) = if constructable { (0, UNDER_CONSTRUCTION) } else { (now, 0) };
                self.cells.push(Cell { x: n(0), z: n(1), item: new.id, start, flags, ..Cell::default() });
            }
            // Plow, plant, clear (updateCell).
            "u" => {
                let (Some(at), Some(new)) = (self.find(n(0), n(1), n(2)), item(n(3))) else { return self.missing(code, args) };
                self.cash -= new.cost;
                self.favor -= new.favor_cost;
                self.experience += new.experience;
                let cell = &mut self.cells[at];
                cell.item = new.id;
                cell.start = if new.maturity == 0 { 0 } else { now };
                cell.flags = 0;
                self.gain_mastery(catalog, new.id, catalog.plant_points, now);
            }
            "h" => match self.find(n(0), n(1), n(2)) {
                Some(at) => self.harvest(catalog, at, now),
                None => self.missing(code, args),
            },
            // Move or turn (moveCell).
            "m" => {
                let Some(at) = self.find(n(0), n(1), n(2)) else { return self.missing(code, args) };
                let cell = &mut self.cells[at];
                (cell.x, cell.z) = (n(3), n(4));
                if n(5) > 0 {
                    cell.item = n(5);
                }
            }
            "s" => {
                let Some(at) = self.find(n(0), n(1), n(2)) else { return self.missing(code, args) };
                let cell = self.cells.remove(at);
                self.cash += catalog.item(cell.item).map_or(0, |sold| sold.selling_price);
            }
            "st" => {
                let Some(at) = self.find(n(0), n(1), n(2)) else { return self.missing(code, args) };
                let cell = self.cells.remove(at);
                self.store(catalog, cell.item, 1, now);
            }
            "ust" => {
                let Some(placed) = item(n(2)) else { return self.unknown_item(code, n(2)) };
                self.unstore(placed.unrotated(), 1);
                let start = if placed.maturity > 0 { now } else { 0 };
                self.cells.push(Cell { x: n(0), z: n(1), item: placed.id, start, ..Cell::default() });
            }
            "f" => {
                let Some(at) = self.find(n(0), n(1), n(2)) else { return self.missing(code, args) };
                let cost = if n(3) > 0 { n(3) } else { self.fertilize_cost(catalog, &self.cells[at], now) };
                self.favor -= cost;
                self.cells[at].flags |= FERTILIZED;
            }
            // Finish a building now (Farm Story's buildings take no time, so it is free).
            "fb" => {
                if let Some(at) = self.find(n(0), n(1), n(2)) {
                    self.cells[at].start = now;
                }
            }
            "fa" => {
                self.favor -= catalog.item(catalog.fertilize_all_item).map_or(0, |all| all.favor_cost);
                for at in 0..self.cells.len() {
                    if self.can_fertilize(catalog, &self.cells[at], now) {
                        self.cells[at].flags |= FERTILIZED;
                    }
                }
            }
            "favc" => {
                for i in 0..n(0).max(0) as usize {
                    if let Some(at) = self.find(n(1 + 3 * i), n(2 + 3 * i), n(3 + 3 * i)) {
                        self.favor -= self.fertilize_cost(catalog, &self.cells[at], now);
                        self.cells[at].flags |= FERTILIZED;
                    }
                }
            }
            // Gather every ripe animal and tree (BoardManagerBase.gather).
            "gaat" | "gawb" => {
                self.favor -= catalog.item(catalog.gather_all_item).map_or(0, |gather| gather.favor_cost);
                for at in 0..self.cells.len() {
                    let category = catalog.item(self.cells[at].item).map_or(0, |found| found.category);
                    if (code == "gawb" || category == 2 || category == 4) && self.ripe(catalog, &self.cells[at], now) {
                        self.harvest(catalog, at, now);
                    }
                }
            }
            "pc" => {
                if let Some(at) = self.find(n(0), n(1), n(2)) {
                    self.cells[at].state += 1;
                }
            }
            // Build a constructable from stored parts (buildConstructable).
            "bc" => {
                let (Some(at), Some(built)) = (self.find(n(0), n(1), n(2)), item(n(2))) else { return self.missing(code, args) };
                self.cells[at].flags &= !UNDER_CONSTRUCTION;
                self.cells[at].start = now;
                for &(part, quantity) in &built.parts {
                    let key = catalog.item(part).map_or(part, Item::unrotated);
                    self.unstore(key, quantity);
                }
                self.experience += built.experience;
            }
            "bp" => {
                let Some(part) = item(n(0)) else { return self.unknown_item(code, n(0)) };
                let quantity = n(1).max(1);
                self.favor -= part.favor_cost * quantity;
                self.cash -= part.cost * quantity;
                self.store(catalog, part.id, quantity, now);
            }
            "bacp" => {
                let Some(built) = item(n(0)) else { return self.unknown_item(code, n(0)) };
                for &(id, quantity) in &built.parts {
                    let Some(part) = item(id) else { continue };
                    let missing = quantity - self.stored_count(part.unrotated());
                    if missing > 0 {
                        self.store(catalog, id, missing, now);
                        self.favor -= part.favor_cost * missing;
                    }
                }
            }
            // A width-0 market item bought in the game (MarketActivityBase.buyItemDirect).
            "bi" => {
                let Some(bought) = item(n(0)) else { return self.unknown_item(code, n(0)) };
                self.favor -= bought.favor_cost;
                self.cash -= bought.cost;
                self.store(catalog, bought.id, n(1).max(1), now);
            }
            "ui" => {
                let Some(unlocked) = item(n(0)) else { return self.unknown_item(code, n(0)) };
                self.favor -= unlocked.unlock_fp;
                self.push_to_list("unlockedItemIds", unlocked.id);
            }
            "usce" => {
                let slots = self.user.get("slotsUnlocked").map_or(0, num);
                self.user.insert("slotsUnlocked".into(), json!(slots + 1));
            }
            // A setting or the tutorial step (utilities/ClientApi).
            "user" => {
                let (Some(setting), Some(value)) = (args.first(), args.get(1)) else { return };
                let value = if *setting == "tutorialState" { json!(value.parse::<i64>().unwrap_or(0)) } else { json!(*value == "1") };
                self.user.insert(setting.to_string(), value);
            }
            "sa" => {
                if let Some(avatar) = args.first() {
                    self.user.insert("avatar".into(), json!(avatar.replace('.', ",")));
                }
            }
            "qc" => {
                if let Some(quest) = catalog.quests.get(&n(0)).cloned() {
                    self.reward(catalog, &quest, now);
                }
                self.push_to_list("completedQuestIds", n(0));
            }
            "sqt" => self.favor -= n(2).max(0),
            // A finished collection (EnsembleRowView): its items go, its reward comes.
            "rec" => {
                let Some(ensemble) = catalog.ensembles.get(&n(0)).cloned() else { return self.unknown_item(code, n(0)) };
                for &(id, quantity) in &ensemble.items {
                    self.unstore(id, quantity);
                }
                self.reward(catalog, &ensemble.reward, now);
                let entry = self.ensemble_mastery.entry(n(0)).or_insert((0, 0));
                entry.1 += 1;
                let (level, progress) = *entry;
                if let Some(rank) = ensemble.masteries.get(level as usize).filter(|rank| progress >= rank.threshold) {
                    *entry = (level + 1, 0);
                    let rank = rank.clone();
                    self.reward(catalog, &rank, now);
                }
            }
            // Analytics, achievements checks, neighbour actions, quest bookkeeping: nothing to keep.
            _ => {}
        }
    }

    fn push_to_list(&mut self, key: &str, id: i64) {
        let list = self.user.entry(key.to_string()).or_insert_with(|| json!([]));
        if !list.is_array() {
            *list = json!([]);
        }
        let list = list.as_array_mut().unwrap();
        if !list.iter().any(|known| num(known) == id) {
            list.push(json!(id));
        }
    }

    fn missing(&self, code: &str, args: &[&str]) {
        crate::log::line(&format!("[offline] storm8: {code} {} has no cell on the saved farm", args.join(",")));
    }

    fn unknown_item(&self, code: &str, id: i64) {
        crate::log::line(&format!("[offline] storm8: {code} names unknown item {id}"));
    }
}
