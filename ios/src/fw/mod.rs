//! Stand-ins for Apple's Foundation and UIKit classes.

pub mod app;
pub mod collections;
pub mod data;
pub mod foundation;
pub mod object;
pub mod paths;
pub mod plist;
pub mod string;

use crate::symbols::Registry;

pub fn register(r: &mut Registry) {
    object::define(r);
    string::register(r);
    collections::register(r);
    data::register(r);
    foundation::register(r);
    app::register(r);
}
