//! Monster Life's Flash menus: fixes to how the game feeds them. Its menu
//! manager (engine::swf::MenuManager) passes the screen size and touches to
//! the menus' Flash player.

use crate::gameswf_blend::call_original;
use crate::runtime::{mem, Args, ArgSource, Ret, Uc};
use std::sync::Mutex;

pub const RESIZE_MENUS: &str = "_ZN6engine3swf11MenuManager13ResizeTextureEii";
const SET_MENUS_VIEWPORT: &str = "_ZN6engine3swf11MenuManager20SetViewportAndBoundsEv";

/// `engine::swf::MenuManager::ResizeTexture(int, int)`: the menus measure the
/// screen here once (ResizeMenus fits their 680-unit height into it), but
/// their screen size is only set at their first draw, and they come here
/// before it, while their file's stand-in size (1:1, so a 16:9 screen looks
/// 1080 units tall) still holds: menus stretched tall, their bottom cut
/// off. So the size is set first, as the first draw would.
pub fn resize_menus(uc: &mut Uc, args: &mut Args) -> Ret {
    let (manager, width, height) = (args.u32(), args.u32(), args.u32());
    let set = crate::runtime::shared().guest_exports.get(SET_MENUS_VIEWPORT).copied();
    if let Some(set) = set {
        let _ = crate::runtime::call_guest(uc, set, &[manager]);
    }
    call_original(uc, RESIZE_MENUS, &[manager, width, height])
}

pub const MENU_INPUT: &str = "_ZN6engine3swf11MenuManager12OnInputEventERKN3glf10InputEventE";

/// glf input event kinds (the event's first halfword).
const DOWN: u16 = 0xD7;
const MOVE: u16 = 0xD8;
const UP: u16 = 0xD9;
const CANCEL: u16 = 0xDA;

/// The touch the menus were told about: its "down"'s time stamp (None:
/// no finger down) and where the finger is now.
#[derive(Default)]
struct Touch {
    down: Option<u32>,
    at: (i16, i16),
}

impl Touch {
    /// Where the menus should see an event of `kind`, stamped `time`, at
    /// `at`: the game's copy of the held touch's "down" (its same time
    /// stamp) is seen where the finger is now.
    fn place(&mut self, kind: u16, time: u32, at: (i16, i16)) -> (i16, i16) {
        match kind {
            DOWN if self.down == Some(time) => return self.at,
            DOWN => self.down = Some(time),
            MOVE => {}
            UP | CANCEL => self.down = None,
            _ => return at,
        }
        self.at = at;
        at
    }
}

static TOUCH: Mutex<Touch> = Mutex::new(Touch { down: None, at: (0, 0) });

/// `engine::swf::MenuManager::OnInputEvent(glf::InputEvent const&)`: the
/// game's input keeps a copy of each touch's "down" and sends it again about
/// 0.15 s later as a long-press notice, even mid-drag. The menus take it for
/// a touch where the finger first went down, and a Flash step before the
/// next move sees the finger back there: a dragged list jumps back for a
/// frame. So the menus see that notice (known by its time stamp, at +8)
/// where the finger is now; the event is put back as it was for whoever
/// gets it next.
pub fn menu_input(uc: &mut Uc, args: &mut Args) -> Ret {
    let (manager, event) = (args.u32(), args.u32());
    let memory = mem();
    let (kind, at) = (memory.read_u16(event), (memory.read_u16(event + 0x18) as i16, memory.read_u16(event + 0x1a) as i16));
    let time = memory.read_u32(event + 8);
    let seen = TOUCH.lock().unwrap_or_else(|e| e.into_inner()).place(kind, time, at);
    if seen == at {
        return call_original(uc, MENU_INPUT, &[manager, event]);
    }
    memory.write_u16(event + 0x18, seen.0 as u16);
    memory.write_u16(event + 0x1a, seen.1 as u16);
    let result = call_original(uc, MENU_INPUT, &[manager, event]);
    memory.write_u16(event + 0x18, at.0 as u16);
    memory.write_u16(event + 0x1a, at.1 as u16);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The game's copy of a "down" (same time stamp, sent while the finger
    /// is still down) is seen where the finger is now; everything else where
    /// it happens. A fresh press after a missed lift is not moved.
    #[test]
    fn a_repeated_down_is_seen_where_the_finger_is() {
        let mut touch = Touch::default();
        let events = [(DOWN, 1, (10, 900)), (MOVE, 2, (10, 850)), (DOWN, 1, (10, 900)), (MOVE, 3, (10, 800)), (UP, 4, (10, 790)),
            (DOWN, 5, (5, 5)), (CANCEL, 6, (5, 5)), (DOWN, 7, (7, 7)), (MOVE, 8, (8, 8)), (DOWN, 9, (60, 60)), (DOWN, 9, (60, 60))];
        let seen: Vec<(i16, i16)> = events.into_iter().map(|(kind, time, at)| touch.place(kind, time, at)).collect();
        assert_eq!(seen, [(10, 900), (10, 850), (10, 850), (10, 800), (10, 790), (5, 5), (5, 5), (7, 7), (8, 8), (60, 60), (60, 60)]);
    }
}
