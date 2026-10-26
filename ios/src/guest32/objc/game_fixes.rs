//! A game's own bugs that an iPhone has too, fixed where the owner asked:
//! each fix follows one game method (`follows`, then `after_message`).

use super::bridge::{Bridge, GuestSide};
use super::functions::Ctx;
use std::sync::Mutex;

/// Whether `-[class name]` is followed by a fix (the class name only made
/// for a selector a fix follows).
pub(super) fn follows(name: &str, class: impl FnOnce() -> String) -> bool {
    name == "dialogViewDidClose:" && class() == "SCAlertController"
}

/// SpellCraft: a story box can line up a next box that shows only once. When
/// that next box was seen before (a quit in the middle of the tutorial),
/// -[SCAlertController processQueue] skips it without telling anyone, and the
/// tutorial, which waits for the next box to close before it goes on
/// (-[SCTutorialController dialogViewDidClose:]), kept the map locked. A box
/// with a next box closed and none is showing or lined up: the tutorial goes
/// on, as it would have when the skipped box closed.
pub(super) fn after_message(bridge: &Mutex<Bridge>, g: &dyn GuestSide, alert: u32, dialog: u32) -> Result<(), String> {
    let c = Ctx { bridge, g };
    let key = {
        let host = crate::fw::string::make("nextDialogName");
        bridge.lock().unwrap_or_else(|e| e.into_inner()).guest_object(g, host)?
    };
    let next = c.send(c.send(dialog, "spec", &[])?, "stringValueForKey:", &[key])?;
    if c.send(next, "length", &[])? == 0 || c.send(alert, "currentAlert", &[])? != 0 || c.send(c.send(alert, "alertQueue", &[])?, "count", &[])? != 0 {
        return Ok(());
    }
    let Some(class) = bridge.lock().unwrap_or_else(|e| e.into_inner()).classes.class_named("SCTutorialController").map(|c| c.address) else {
        return Ok(());
    };
    let tutorial = c.send(class, "tutorialController", &[])?;
    if c.send(tutorial, "tutorialUnderway", &[])? & 0xff != 0 {
        c.send(tutorial, "beginStageInteraction", &[])?;
    }
    Ok(())
}
