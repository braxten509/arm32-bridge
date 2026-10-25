//! UIPageControl and UIPickerView: they keep their state as UIKit does,
//! are drawn (fw/pictures.rs, from `pieces`) and follow taps.
//!
//! A page control shows a dot per page, the current one brighter; a tap on
//! either side of the current dot moves one page that way and sends
//! UIControlEventValueChanged. A picker shows each component's rows (the
//! titles its delegate gives) around the selected one, between two lines;
//! a tap on a row above or below selects it and tells the delegate
//! (pickerView:didSelectRow:inComponent:), as a flick to it would.

use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::graphics::TextLook;
use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property, property_get, property_set, view_state};
use crate::fw::string;
use crate::objc::runtime::{self, Id, Sel};
use std::collections::HashMap;
use std::sync::Mutex;

/// Something a page control or picker shows, in its own points.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Color([f64; 4], Rect),
    /// A round dot filling the rect.
    Dot([f64; 4], Rect),
    Text(TextLook, Rect),
}

fn color_of(color: Id, default: [f64; 4]) -> [f64; 4] {
    crate::fw::graphics::color_rgba(color).unwrap_or(default)
}

fn size_of(view: Id) -> Size {
    view_state(view).frame.size
}

// ----- UIPageControl -----

/// UIControlEventValueChanged.
const VALUE_CHANGED: usize = 1 << 12;
const DOT: f64 = 7.0;
/// From one dot's left edge to the next one's.
const DOT_PITCH: f64 = 16.0;
const PAGE_CONTROL_HEIGHT: f64 = 36.0;

property!(pages, set_pages_raw, "numberOfPages", isize, 0);
property!(current_page, set_current_raw, "currentPage", isize, 0);
property!(hides_single, set_hides_single, "hidesForSinglePage", bool, false);
property!(defers, set_defers, "defersCurrentPageDisplay", bool, false);
property!(dot_color, set_dot_color, "pageIndicatorTintColor", id, 0);
property!(current_dot_color, set_current_dot_color, "currentPageIndicatorTintColor", id, 0);

fn clamp_page(page: isize, pages: isize) -> isize {
    page.clamp(0, (pages - 1).max(0))
}

/// The page whose dot shows bright (with defersCurrentPageDisplay, the
/// last one shown until updateCurrentPageDisplay).
fn shown_page(this: Id) -> isize {
    if defers(this, 0) {
        property_get(this, "displayedPage").map_or_else(|| current_page(this, 0), |v| v as isize)
    } else {
        current_page(this, 0)
    }
}

extern "C" fn set_current_page(this: Id, s: Sel, page: isize) {
    set_current_raw(this, s, clamp_page(page, pages(this, 0)));
    property_set(this, "displayedPage", current_page(this, 0) as u64);
}

extern "C" fn set_pages(this: Id, s: Sel, count: isize) {
    set_pages_raw(this, s, count.max(0));
    set_current_page(this, s, current_page(this, 0));
}

extern "C" fn update_display(this: Id, _: Sel) {
    property_set(this, "displayedPage", current_page(this, 0) as u64);
}

fn dots_width(count: isize) -> f64 {
    if count <= 0 { 0.0 } else { count as f64 * DOT_PITCH - (DOT_PITCH - DOT) }
}

extern "C" fn size_for_pages(_this: Id, _: Sel, count: isize) -> Size {
    Size { width: dots_width(count), height: PAGE_CONTROL_HEIGHT }
}

extern "C" fn page_size_that_fits(this: Id, _: Sel, _size: Size) -> Size {
    size_for_pages(this, 0, pages(this, 0))
}

/// The dot rects, centred in the bounds.
fn dot_rects(count: isize, size: Size) -> Vec<Rect> {
    let left = (size.width - dots_width(count)) / 2.0;
    let top = (size.height - DOT) / 2.0;
    (0..count.max(0)).map(|i| Rect::new(left + i as f64 * DOT_PITCH, top, DOT, DOT)).collect()
}

fn page_pieces(this: Id) -> Vec<Piece> {
    let count = pages(this, 0);
    if count == 0 || (count == 1 && hides_single(this, 0)) {
        return Vec::new();
    }
    let shown = shown_page(this);
    let (dim, bright) = (color_of(dot_color(this, 0), [1.0, 1.0, 1.0, 0.3]), color_of(current_dot_color(this, 0), [1.0, 1.0, 1.0, 1.0]));
    dot_rects(count, size_of(this)).into_iter().enumerate().map(|(i, rect)| Piece::Dot(if i as isize == shown { bright } else { dim }, rect)).collect()
}

/// Where a tap at `x` moves the page: one to the side of the current dot it is on.
fn page_after_tap(current: isize, count: isize, x: f64, size: Size) -> isize {
    let Some(dot) = dot_rects(count, size).get(current as usize).copied() else { return current };
    let middle = dot.origin.x + DOT / 2.0;
    clamp_page(if x < middle { current - 1 } else { current + 1 }, count)
}

/// UIControl's tracking ends with the finger up: a tap inside turns the page.
extern "C" fn page_end_tracking(this: Id, _: Sel, touch: Id, _event: Id) {
    let at = crate::fw::touch::location_in(touch, this);
    let size = size_of(this);
    if at.x < 0.0 || at.y < 0.0 || at.x >= size.width || at.y >= size.height {
        return;
    }
    let (current, count) = (current_page(this, 0), pages(this, 0));
    let next = page_after_tap(current, count, at.x, size);
    if next != current {
        set_current_raw(this, 0, next);
        if !defers(this, 0) {
            property_set(this, "displayedPage", next as u64);
        }
        runtime::send(this, "sendActionsForControlEvents:", &[VALUE_CHANGED]);
    }
}

// ----- UIPickerView -----

const ROW_HEIGHT: f64 = 32.0;
const PICKER_SIZE: Size = Size { width: 320.0, height: 216.0 };
/// Space at each side and between components.
const COMPONENT_GAP: f64 = 8.0;
const TITLE_SIZE: f64 = 21.0;

#[derive(Default)]
struct Picker {
    /// Rows per component, as the data source last said.
    rows: Vec<isize>,
    selected: Vec<isize>,
    loaded: bool,
}

static PICKERS: Mutex<Option<HashMap<Id, Picker>>> = Mutex::new(None);

fn with<R>(picker: Id, f: impl FnOnce(&mut Picker) -> R) -> R {
    f(PICKERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(picker).or_default())
}

property!(source, set_source, "dataSource", id, 0);
property!(picker_delegate, set_picker_delegate, "delegate", id, 0);
property!(shows_indicator, set_shows_indicator, "showsSelectionIndicator", bool, false);

fn ask(receiver: Id, name: &str, args: &[usize]) -> Option<usize> {
    (receiver != 0 && runtime::responds(receiver, name)).then(|| runtime::send(receiver, name, args))
}

/// Asks the picker's delegate for a CGFloat about a component.
fn ask_f64(picker: Id, name: &str, component: usize) -> Option<f64> {
    let delegate = picker_delegate(picker, 0);
    let s = runtime::sel(name);
    let imp = runtime::imp_for(delegate, s).filter(|_| delegate != 0)?;
    let f: extern "C" fn(Id, Sel, Id, usize) -> f64 = unsafe { std::mem::transmute(imp) };
    Some(f(delegate, s, picker, component)).filter(|v| v.is_finite())
}

/// Asks the data source for the components and their rows (all, or one).
fn load(picker: Id, only: Option<usize>) {
    let source = source(picker, 0);
    let components = ask(source, "numberOfComponentsInPickerView:", &[picker]).map_or(0, |n| n as isize).max(0) as usize;
    let counts: Vec<(usize, isize)> = (0..components)
        .filter(|&c| only.is_none_or(|o| o == c))
        .map(|c| (c, ask(source, "pickerView:numberOfRowsInComponent:", &[picker, c]).map_or(0, |n| n as isize).max(0)))
        .collect();
    with(picker, |p| {
        p.rows.resize(components, 0);
        p.selected.resize(components, 0);
        for (c, rows) in counts {
            p.rows[c] = rows;
            p.selected[c] = p.selected[c].clamp(0, (rows - 1).max(0));
        }
        p.loaded = true;
    });
}

fn ensure_loaded(picker: Id) {
    if !with(picker, |p| p.loaded) {
        load(picker, None);
    }
}

extern "C" fn reload_all(this: Id, _: Sel) {
    load(this, None);
}

extern "C" fn reload_component(this: Id, _: Sel, component: isize) {
    if component >= 0 {
        load(this, Some(component as usize));
    }
}

extern "C" fn number_of_components(this: Id, _: Sel) -> isize {
    ensure_loaded(this);
    with(this, |p| p.rows.len() as isize)
}

extern "C" fn rows_in_component(this: Id, _: Sel, component: isize) -> isize {
    ensure_loaded(this);
    with(this, |p| p.rows.get(component as usize).copied().unwrap_or(0))
}

/// -1 when the component has no rows, as UIKit answers.
extern "C" fn selected_row(this: Id, _: Sel, component: isize) -> isize {
    ensure_loaded(this);
    with(this, |p| match (p.rows.get(component as usize), p.selected.get(component as usize)) {
        (Some(&rows), Some(&row)) if rows > 0 => row,
        _ => -1,
    })
}

/// Selects a row (kept within the component's rows); the delegate isn't told, as in UIKit.
extern "C" fn select_row(this: Id, _: Sel, row: isize, component: isize, _animated: u8) {
    ensure_loaded(this);
    with(this, |p| {
        if let (Some(&rows), Some(selected)) = (p.rows.get(component as usize), p.selected.get_mut(component as usize)) {
            *selected = row.clamp(0, (rows - 1).max(0));
        }
    });
}

fn row_height(picker: Id, component: usize) -> f64 {
    ask_f64(picker, "pickerView:rowHeightForComponent:", component).filter(|&h| h > 0.0).unwrap_or(ROW_HEIGHT)
}

extern "C" fn row_size(this: Id, _: Sel, component: isize) -> Size {
    let columns = columns(this);
    let width = columns.get(component as usize).map_or(0.0, |&(_, w)| w);
    Size { width, height: row_height(this, component.max(0) as usize) }
}

extern "C" fn picker_size_that_fits(_this: Id, _: Sel, _size: Size) -> Size {
    PICKER_SIZE
}

/// Each component's left edge and width: the delegate's widths, else equal shares.
fn columns(picker: Id) -> Vec<(f64, f64)> {
    let count = with(picker, |p| p.rows.len());
    let size = size_of(picker);
    let share = ((size.width - COMPONENT_GAP * (count as f64 + 1.0)) / count.max(1) as f64).max(0.0);
    let widths: Vec<f64> = (0..count).map(|c| ask_f64(picker, "pickerView:widthForComponent:", c).unwrap_or(share)).collect();
    let total: f64 = widths.iter().sum::<f64>() + COMPONENT_GAP * (count as f64 - 1.0).max(0.0);
    let mut x = (size.width - total) / 2.0;
    widths
        .into_iter()
        .map(|w| {
            let column = (x, w);
            x += w + COMPONENT_GAP;
            column
        })
        .collect()
}

fn title(picker: Id, row: isize, component: usize) -> String {
    let delegate = picker_delegate(picker, 0);
    let args = [picker, row as usize, component];
    if let Some(text) = ask(delegate, "pickerView:titleForRow:forComponent:", &args) {
        return if text == 0 { String::new() } else { string::get(text) };
    }
    match ask(delegate, "pickerView:attributedTitleForRow:forComponent:", &args) {
        Some(text) if text != 0 => string::get(runtime::send(text, "string", &[])),
        _ => {
            if ask(delegate, "pickerView:viewForRow:forComponent:reusingView:", &[picker, row as usize, component, 0]).is_some() {
                crate::fw::controls::not_drawn("UIPickerView row views");
            }
            String::new()
        }
    }
}

/// The selected row's band: centred in the picker.
fn middle_band(size: Size, height: f64) -> Rect {
    Rect::new(0.0, (size.height - height) / 2.0, size.width, height)
}

fn picker_pieces(this: Id) -> Vec<Piece> {
    ensure_loaded(this);
    let size = size_of(this);
    let (rows, selected) = with(this, |p| (p.rows.clone(), p.selected.clone()));
    let mut out = Vec::new();
    let line = [0.78, 0.78, 0.8, 1.0];
    let band = middle_band(size, row_height(this, 0));
    out.push(Piece::Color(line, Rect::new(0.0, band.origin.y, size.width, 0.5)));
    out.push(Piece::Color(line, Rect::new(0.0, band.origin.y + band.size.height - 0.5, size.width, 0.5)));
    for (c, &(left, width)) in columns(this).iter().enumerate() {
        let height = row_height(this, c);
        let middle = (size.height - height) / 2.0;
        let reach = (middle / height).ceil() as isize + 1;
        for offset in -reach..=reach {
            let row = selected[c] + offset;
            let top = middle + offset as f64 * height;
            if row < 0 || row >= rows[c] || top + height <= 0.0 || top >= size.height {
                continue;
            }
            let text = title(this, row, c);
            if text.is_empty() {
                continue;
            }
            let look = TextLook {
                text,
                font: (crate::fw::controls::font_name_and_size(0).0, TITLE_SIZE),
                color: [0.0, 0.0, 0.0, if offset == 0 { 1.0 } else { 0.45 }],
                shadow: None,
                alignment: 1,
                line_break: 4,
                lines: 1,
                size: (width, height),
                minimum_font_size: None,
            };
            out.push(Piece::Text(look, Rect::new(left, top, width, height)));
        }
    }
    out
}

/// The row a tap at `at` lands on, as (component, row), when it is a row
/// other than the selected one.
fn tapped_row(columns: &[(f64, f64)], rows: &[isize], selected: &[isize], heights: &[f64], size: Size, at: Point) -> Option<(usize, isize)> {
    let c = columns.iter().position(|&(left, width)| at.x >= left && at.x < left + width)?;
    let height = heights[c];
    let middle = (size.height - height) / 2.0;
    let offset = ((at.y - middle) / height).floor() as isize;
    let row = selected[c] + offset;
    (offset != 0 && row >= 0 && row < rows[c]).then_some((c, row))
}

extern "C" fn picker_touches_ended(this: Id, _: Sel, touches: Id, _event: Id) {
    let touch = runtime::send(touches, "anyObject", &[]);
    let at = crate::fw::touch::location_in(touch, this);
    ensure_loaded(this);
    let (rows, selected) = with(this, |p| (p.rows.clone(), p.selected.clone()));
    let heights: Vec<f64> = (0..rows.len()).map(|c| row_height(this, c)).collect();
    if let Some((component, row)) = tapped_row(&columns(this), &rows, &selected, &heights, size_of(this), at) {
        with(this, |p| p.selected[component] = row);
        ask(picker_delegate(this, 0), "pickerView:didSelectRow:inComponent:", &[this, row as usize, component]);
    }
}

extern "C" fn ignore_touches(_this: Id, _: Sel, _touches: Id, _event: Id) {}

extern "C" fn picker_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    let s = runtime::sel("initWithFrame:");
    let view = runtime::class_named("UIView"); // before taking the lock: it locks too
    if let Some(imp) = runtime::lock().lookup(view, s) {
        let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
        f(this, s, frame);
    }
    // A picker takes touches (UIView's default).
    property_set(this, "userInteraction", 1);
    this
}

/// What a page control or picker shows (None for other views).
pub fn pieces(view: Id) -> Option<Vec<Piece>> {
    let is = |name: &str| {
        let class = runtime::class_named(name);
        let rt = runtime::lock();
        class != 0 && rt.is_subclass(rt.class_of(view), class)
    };
    if is("UIPageControl") {
        Some(page_pieces(view))
    } else if is("UIPickerView") {
        Some(picker_pieces(view))
    } else {
        None
    }
}

pub fn register() {
    let page = define_class("UIPageControl", "UIControl", 16);
    methods(
        page,
        &[
            ("numberOfPages", pages as usize),
            ("setNumberOfPages:", set_pages as usize),
            ("currentPage", current_page as usize),
            ("setCurrentPage:", set_current_page as usize),
            ("hidesForSinglePage", hides_single as usize),
            ("setHidesForSinglePage:", set_hides_single as usize),
            ("defersCurrentPageDisplay", defers as usize),
            ("setDefersCurrentPageDisplay:", set_defers as usize),
            ("updateCurrentPageDisplay", update_display as usize),
            ("pageIndicatorTintColor", dot_color as usize),
            ("setPageIndicatorTintColor:", set_dot_color as usize),
            ("currentPageIndicatorTintColor", current_dot_color as usize),
            ("setCurrentPageIndicatorTintColor:", set_current_dot_color as usize),
            ("sizeForNumberOfPages:", size_for_pages as usize),
            ("sizeThatFits:", page_size_that_fits as usize),
            ("endTrackingWithTouch:withEvent:", page_end_tracking as usize),
        ],
    );
    let picker = define_class("UIPickerView", "UIView", 16);
    methods(
        picker,
        &[
            ("initWithFrame:", picker_init_with_frame as usize),
            ("dataSource", source as usize),
            ("setDataSource:", set_source as usize),
            ("delegate", picker_delegate as usize),
            ("setDelegate:", set_picker_delegate as usize),
            ("showsSelectionIndicator", shows_indicator as usize),
            ("setShowsSelectionIndicator:", set_shows_indicator as usize),
            ("numberOfComponents", number_of_components as usize),
            ("numberOfRowsInComponent:", rows_in_component as usize),
            ("reloadAllComponents", reload_all as usize),
            ("reloadComponent:", reload_component as usize),
            ("selectRow:inComponent:animated:", select_row as usize),
            ("selectedRowInComponent:", selected_row as usize),
            ("rowSizeForComponent:", row_size as usize),
            ("sizeThatFits:", picker_size_that_fits as usize),
            ("touchesBegan:withEvent:", ignore_touches as usize),
            ("touchesMoved:withEvent:", ignore_touches as usize),
            ("touchesEnded:withEvent:", picker_touches_ended as usize),
            ("touchesCancelled:withEvent:", ignore_touches as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dots are centred and a tap moves one page toward its side of the
    /// current dot, never past the ends.
    #[test]
    fn page_taps_move_one_page_toward_their_side() {
        let size = Size { width: 100.0, height: 36.0 };
        let dots = dot_rects(3, size);
        assert_eq!(dots[0], Rect::new(100.0 / 2.0 - 39.0 / 2.0, 14.5, 7.0, 7.0));
        assert_eq!(dots[2].origin.x + DOT - dots[0].origin.x, 39.0);
        assert_eq!(page_after_tap(1, 3, 10.0, size), 0);
        assert_eq!(page_after_tap(1, 3, 90.0, size), 2);
        assert_eq!(page_after_tap(2, 3, 90.0, size), 2);
        assert_eq!(page_after_tap(0, 3, 0.0, size), 0);
        assert_eq!(clamp_page(7, 3), 2);
        assert_eq!(clamp_page(-1, 0), 0);
    }

    /// A tap lands on the row above or below the selected one in the
    /// component under it; the selected row and rows past the ends don't count.
    #[test]
    fn picker_taps_find_their_row() {
        let size = Size { width: 320.0, height: 216.0 };
        let columns = [(8.0, 148.0), (164.0, 148.0)];
        let (rows, selected, heights) = ([10, 3], [5, 0], [32.0, 32.0]);
        let middle = (216.0 - 32.0) / 2.0;
        let tap = |x: f64, y: f64| tapped_row(&columns, &rows, &selected, &heights, size, Point { x, y });
        assert_eq!(tap(50.0, middle + 40.0), Some((0, 6)));
        assert_eq!(tap(50.0, middle - 70.0), Some((0, 2)));
        assert_eq!(tap(50.0, middle + 10.0), None, "the selected row");
        assert_eq!(tap(200.0, middle - 10.0), None, "above row 0");
        assert_eq!(tap(200.0, middle + 33.0), Some((1, 1)));
        assert_eq!(tap(2.0, middle + 40.0), None, "between components");
    }
}
