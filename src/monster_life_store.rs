//! Monster Life's currency store, offline: its real-money packs are gone, so
//! its Crystals and Friend Stones tabs sell packs for Gold instead (the
//! store's Flash menu, menus_HardCashShop.swf, lists them; its Gold tab is
//! hidden). A pack's buy button sends "gb:<gold>:<currency>:<amount>" to the
//! game's buy native, and here that much Gold is taken and the pack given,
//! through the game's own currency holders (so its goals, sounds, top bar and
//! save hear of it as of any other purchase).

use crate::gameswf_blend::call_original;
use crate::runtime::{mem, Args, ArgSource, Ret, Uc};

/// What a gold pack costs and gives.
#[derive(Debug, PartialEq)]
pub struct Trade {
    pub gold: u32,
    pub currency: Currency,
    pub amount: u32,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Currency {
    Crystals,
    FriendStones,
}

/// The pack a buy button asks for, from its id ("gb:1800:1:30": 1800 Gold
/// for 30 Crystals; currency 1 Crystals, 2 Friend Stones). None for the
/// store's own (real-money) items and anything malformed.
pub fn parse_trade(uid: &str) -> Option<Trade> {
    let mut parts = uid.strip_prefix("gb:")?.split(':');
    let mut number = || parts.next()?.parse::<u32>().ok().filter(|&n| n > 0 && n <= i32::MAX as u32);
    let (gold, currency, amount) = (number()?, number()?, number()?);
    let currency = match currency {
        1 => Currency::Crystals,
        2 => Currency::FriendStones,
        _ => return None,
    };
    parts.next().is_none().then_some(Trade { gold, currency, amount })
}

pub const BUY_ITEM: &str = "_ZN4game12flashNatives20gameHardCurrencyShop29NativeHardCurrencyShopBuyItemERKN7gameswf12FunctionCallE";
const GAME_INSTANCE: &str = "_ZN6engine4main4Game11GetInstanceEv";
const VALUE_TEXT: &str = "_ZNK7gameswf7ASValue8toStringEv";
const REMOVE: &str = "_ZN4game6player14PlayerResourceIiE6RemoveEi";
/// GetInstance's place in libMonsterLife.so 1.1.5, to find the library's
/// start, and the place of PlayerResource<int>::Add there (not exported),
/// with its first instructions (push {r4-r8,r10,lr}; subs r7,r1,#0) to make
/// sure it is that build.
const GAME_INSTANCE_AT: u32 = 0x2f_7ed4;
const ADD_AT: u32 = 0x68_3ee4;
const ADD_STARTS: [u32; 2] = [0xe92d_45f0, 0xe251_7000];
/// Game::GetInstance()'s player, and that player's Gold, Crystals and Friend
/// Stones (game::player::Player::GetCurrencyByType).
const PLAYER: u32 = 0x1_77b4;
const GOLD: u32 = 0x1c;
const CRYSTALS: u32 = 0x64;
const FRIEND_STONES: u32 = 0xac;

/// `game::flashNatives::gameHardCurrencyShop::NativeHardCurrencyShopBuyItem`
/// (the store's buy button, `(uid, currency)`): a gold pack is paid for here
/// and the menu hears true (bought) or false (not enough Gold); the store's
/// own items go to the game as before.
pub fn buy_item(uc: &mut Uc, args: &mut Args) -> Ret {
    let call = args.u32();
    let memory = mem();
    let (env, first) = (memory.read_u32(call + 0xc), memory.read_u32(call + 0x14));
    let uid_value = memory.read_u32(env) + first * 12;
    let uid = exported(VALUE_TEXT).and_then(|text| crate::runtime::call_guest(uc, text, &[uid_value]).ok()).map(|string| gameswf_string(string as u32));
    let Some(trade) = uid.as_deref().and_then(parse_trade) else {
        return call_original(uc, BUY_ITEM, &[call]);
    };
    let bought = pay(uc, &trade).unwrap_or_else(|why| {
        crate::log::line(&format!("[store] {trade:?} not bought: {why}"));
        false
    });
    crate::log::line(&format!("[store] {trade:?}: {}", if bought { "bought" } else { "not enough gold" }));
    // The call's answer (a gameswf::ASValue): a boolean (type 1, value at +4).
    let result = memory.read_u32(call);
    if result != 0 && memory.read_u8(result) == 0 {
        memory.write_u8(result, 1);
        memory.write_u8(result + 4, bought as u8);
    }
    Ret::Void
}

/// Takes the pack's Gold and gives the pack; false when the Gold is short
/// (the game's Remove refuses then, with its "not enough" sound).
fn pay(uc: &mut Uc, trade: &Trade) -> Result<bool, String> {
    let instance = exported(GAME_INSTANCE).ok_or("no Game::GetInstance")?;
    let add = instance - GAME_INSTANCE_AT + ADD_AT;
    let memory = mem();
    if [memory.read_u32(add), memory.read_u32(add + 4)] != ADD_STARTS {
        return Err(format!("PlayerResource<int>::Add is not at {add:#x} (another game build?)"));
    }
    let game = crate::runtime::call_guest(uc, instance, &[])? as u32;
    let player = memory.read_u32(game + PLAYER);
    if player == 0 {
        return Err("no player yet".into());
    }
    let remove = exported(REMOVE).ok_or("no PlayerResource<int>::Remove")?;
    if crate::runtime::call_guest(uc, remove, &[player + GOLD, trade.gold])? as u32 & 0xff == 0 {
        return Ok(false);
    }
    let holder = match trade.currency {
        Currency::Crystals => CRYSTALS,
        Currency::FriendStones => FRIEND_STONES,
    };
    crate::runtime::call_guest(uc, add, &[player + holder, trade.amount])?;
    Ok(true)
}

fn exported(symbol: &str) -> Option<u32> {
    crate::runtime::shared().guest_exports.get(symbol).copied()
}

/// A gameswf::String's text: its first byte is 0xff when the text is kept
/// elsewhere (pointer at +0xc), else the text follows it.
fn gameswf_string(string: u32) -> String {
    let memory = mem();
    let text = if memory.read_u8(string) == 0xff { memory.read_u32(string + 0xc) } else { string + 1 };
    let bytes: Vec<u8> = (0..256).map(|i| memory.read_u8(text + i)).take_while(|&b| b != 0).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gold pack's id gives its price and contents; real-money ids, other
    /// currencies, zero or negative numbers and extra parts give nothing.
    #[test]
    fn gold_pack_ids_are_read() {
        assert_eq!(parse_trade("gb:1800:1:30"), Some(Trade { gold: 1800, currency: Currency::Crystals, amount: 30 }));
        assert_eq!(parse_trade("gb:300:2:25"), Some(Trade { gold: 300, currency: Currency::FriendStones, amount: 25 }));
        for bad in ["com.gameloft.crystals_1", "gb:1800:0:30", "gb:1800:3:30", "gb:0:1:30", "gb:1800:1:0", "gb:-5:1:30", "gb:1800:1:30:9", "gb:1800:1", ""] {
            assert_eq!(parse_trade(bad), None, "{bad}");
        }
    }
}
