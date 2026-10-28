//! Tables showing their rows: the data source's cells placed one under
//! the other at the delegate's heights, their labels laid out by the cell
//! style, drawn selected or not (fw/pictures.rs asks `cell_fill`), and a
//! tap on a row selecting it through the delegate, as UIKit does.
//! Prototype cells from storyboards and registerNib: make new cells from
//! their nibs.

use crate::fw::controls::{self, text_size};
use crate::fw::geometry::{Point, Rect};
use crate::fw::object::methods;
use crate::fw::string;
use crate::fw::uikit::{property_get, property_set, view_state};
use crate::fw::collections;
use crate::objc::runtime::{self, lock, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

fn is(object: Id, name: &str) -> bool {
    let class = runtime::class_named(name);
    object != 0 && class != 0 && {
        let rt = lock();
        rt.is_subclass(rt.class_of(object), class)
    }
}

fn get_id(object: Id, key: &'static str) -> Id {
    property_get(object, key).unwrap_or(0) as Id
}

fn flag(object: Id, key: &'static str, default: bool) -> bool {
    property_get(object, key).map_or(default, |v| v != 0)
}

fn set_frame(view: Id, frame: Rect) {
    let s = runtime::sel("setFrame:");
    if let Some(imp) = runtime::imp_for(view, s) {
        let f: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
        f(view, s, frame);
    }
}

fn frame_of(view: Id) -> Rect {
    view_state(view).frame
}

/// The delegate's answer to a message with a CGFloat result, if it has the method.
fn ask_f64(receiver: Id, name: &str, a: usize, b: usize) -> Option<f64> {
    let s = runtime::sel(name);
    let imp = runtime::imp_for(receiver, s).filter(|_| receiver != 0)?;
    let f: extern "C" fn(Id, Sel, usize, usize) -> f64 = unsafe { std::mem::transmute(imp) };
    Some(f(receiver, s, a, b))
}

fn tell(receiver: Id, name: &str, args: &[usize]) -> Option<usize> {
    (receiver != 0 && runtime::responds(receiver, name)).then(|| runtime::send(receiver, name, args))
}

// ----- prototype cells and registered nibs -----

/// (table, reuse identifier) -> (nib, its external objects).
static NIBS: Mutex<Option<HashMap<(Id, String), (Id, Id)>>> = Mutex::new(None);

pub fn register_nib(table: Id, identifier: &str, nib: Id, externals: Id) {
    crate::fw::counted::retain(nib);
    NIBS.lock().unwrap().get_or_insert_with(HashMap::new).insert((table, identifier.to_string()), (nib, externals));
}

/// A new cell from the nib registered for `identifier`, or nil.
pub fn cell_from_nib(table: Id, identifier: &str) -> Id {
    let Some((nib, externals)) = NIBS.lock().unwrap().as_ref().and_then(|m| m.get(&(table, identifier.to_string())).copied()) else {
        return 0;
    };
    let options = if externals == 0 { 0 } else { collections::dictionary_from(&[(string::make("UINibExternalObjects"), externals)]) };
    let objects = collections::array_items(runtime::send(nib, "instantiateWithOwner:options:", &[0, options]));
    objects.into_iter().find(|&object| is(object, "UITableViewCell")).unwrap_or(0)
}

extern "C" fn table_register_nib(this: Id, _: Sel, nib: Id, identifier: Id) {
    if identifier != 0 {
        register_nib(this, &string::get(identifier), nib, 0);
    }
}

/// Cells whose rows scrolled out of sight (or were reloaded), per table and
/// reuse identifier, waiting to be dequeued again.
static REUSE: Mutex<Option<HashMap<(Id, String), Vec<Id>>>> = Mutex::new(None);

/// A cell leaves the table and waits for reuse under its reuse identifier.
fn put_aside(table: Id, cell: Id) {
    let identifier = get_id(cell, "reuseIdentifier");
    // The reuse queue holds it (as UIKit's), before its superview lets go.
    if identifier != 0 {
        crate::fw::counted::retain(cell);
    }
    runtime::send(cell, "removeFromSuperview", &[]);
    if identifier == 0 {
        return;
    }
    REUSE.lock().unwrap().get_or_insert_with(HashMap::new).entry((table, string::get(identifier))).or_default().push(cell);
}

/// A cell for `identifier`: one waiting for reuse (told -prepareForReuse),
/// else a new one from its nib or its registered class, or nil, as UIKit
/// gives when neither is registered (the game then makes its own).
fn make_cell(table: Id, identifier: Id) -> Id {
    let name = if identifier == 0 { String::new() } else { string::get(identifier) };
    let waiting = REUSE.lock().unwrap().as_mut().and_then(|m| m.get_mut(&(table, name.clone()))).and_then(|list| list.pop());
    if let Some(cell) = waiting {
        // Out of the queue, handed back autoreleased (+0), as UIKit's.
        crate::fw::counted::autorelease(cell);
        if runtime::responds(cell, "prepareForReuse") {
            runtime::send(cell, "prepareForReuse", &[]);
        }
        return cell;
    }
    match cell_from_nib(table, &name) {
        0 => match controls::registered_class(table, &format!("cell:{name}")) {
            Some(class) => runtime::send(runtime::send(class, "alloc", &[]), "initWithStyle:reuseIdentifier:", &[0, identifier]),
            None => 0,
        },
        cell => cell,
    }
}

extern "C" fn table_dequeue(this: Id, _: Sel, identifier: Id) -> Id {
    make_cell(this, identifier)
}

extern "C" fn table_dequeue_for_path(this: Id, _: Sel, identifier: Id, _path: Id) -> Id {
    let cell = make_cell(this, identifier);
    if cell == 0 {
        crate::log!("table {this:#x}: no cell class or nib registered for \"{}\"", if identifier == 0 { String::new() } else { string::get(identifier) });
    }
    cell
}

// ----- the rows on show -----

/// The cells each table shows, with their index paths, top to bottom.
static ROWS: Mutex<Option<HashMap<Id, Vec<(Id, Id)>>>> = Mutex::new(None);

fn rows_of(table: Id) -> Vec<(Id, Id)> {
    ROWS.lock().unwrap().as_ref().and_then(|m| m.get(&table).cloned()).unwrap_or_default()
}

fn section_count(table: Id) -> usize {
    tell(get_id(table, "dataSource"), "numberOfSectionsInTableView:", &[table]).map_or(1, |n| n as isize).max(0) as usize
}

fn row_count(table: Id, section: usize) -> usize {
    tell(get_id(table, "dataSource"), "tableView:numberOfRowsInSection:", &[table, section]).map_or(0, |n| n as isize).max(0) as usize
}

fn row_height(table: Id, path: Id) -> f64 {
    let delegate = get_id(table, "delegate");
    ask_f64(delegate, "tableView:heightForRowAtIndexPath:", table, path)
        .unwrap_or_else(|| property_get(table, "rowHeight").map_or(44.0, f64::from_bits))
}

/// Each table's rows: index path, top and height (content points), top to bottom.
static PLACES: Mutex<Option<HashMap<Id, Vec<(Id, f64, f64)>>>> = Mutex::new(None);

/// Asks the delegate every row's height first (UIKit's row data), placing
/// the rows one under the other after the header view, the footer view
/// after them; the table's content is as tall as they are. Then the rows
/// on show get their cells (`show_rows`); the cells shown before wait for reuse.
pub fn load_rows(table: Id) {
    let source = get_id(table, "dataSource");
    for (cell, _) in rows_of(table) {
        put_aside(table, cell);
    }
    ROWS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&table);
    PLACES.lock().unwrap().get_or_insert_with(HashMap::new).remove(&table);
    property_set(table, "needsReload", 0);
    if source == 0 {
        return;
    }
    property_set(table, "rowsLoaded", 1);
    let width = frame_of(table).size.width;
    let mut y = 0.0;
    let header = get_id(table, "tableHeaderView");
    if header != 0 {
        let height = frame_of(header).size.height;
        set_frame(header, Rect::new(0.0, 0.0, width, height));
        runtime::send(table, "addSubview:", &[header]);
        y += height;
    }
    let mut places = Vec::new();
    for section in 0..section_count(table) {
        for row in 0..row_count(table, section) {
            let path = runtime::send(runtime::class_named("NSIndexPath"), "indexPathForRow:inSection:", &[row, section]);
            crate::fw::counted::retain(path);
            let height = row_height(table, path);
            places.push((path, y, height));
            y += height;
        }
    }
    PLACES.lock().unwrap().get_or_insert_with(HashMap::new).insert(table, places);
    let footer = get_id(table, "tableFooterView");
    if footer != 0 {
        let height = frame_of(footer).size.height;
        set_frame(footer, Rect::new(0.0, y, width, height));
        runtime::send(table, "addSubview:", &[footer]);
        y += height;
    }
    property_set(table, "contentSize.w", width.to_bits());
    property_set(table, "contentSize.h", y.to_bits());
    show_rows(table);
}

/// The rows inside the table's bounds have cells (asked of the data source
/// as they come into view); cells of rows that left it wait for reuse.
fn show_rows(table: Id) {
    let source = get_id(table, "dataSource");
    let places = PLACES.lock().unwrap().as_ref().and_then(|m| m.get(&table).cloned()).unwrap_or_default();
    let state = view_state(table);
    let (top, bottom) = (state.bounds_origin.y, state.bounds_origin.y + state.frame.size.height);
    let width = state.frame.size.width;
    let delegate = get_id(table, "delegate");
    // The rows inside the bounds, worked out once (a big table has many rows).
    let visible: Vec<(Id, f64, f64)> = places.iter().copied().filter(|&(_, y, height)| y + height > top && y < bottom).collect();
    // Rows that left the bounds give their cells back first, so the rows
    // coming in can reuse them.
    let mut old = Vec::new();
    for (cell, path) in rows_of(table) {
        if visible.iter().any(|&(p, _, _)| p == path) {
            old.push((cell, path));
        } else {
            put_aside(table, cell);
            tell(delegate, "tableView:didEndDisplayingCell:forRowAtIndexPath:", &[table, cell, path]);
        }
    }
    let selection = controls::selected(table);
    let mut shown = Vec::new();
    for &(path, y, height) in &visible {
        if let Some(&(cell, _)) = old.iter().find(|&&(_, p)| p == path) {
            shown.push((cell, path));
            continue;
        }
        if source == 0 {
            continue;
        }
        let cell = runtime::send(source, "tableView:cellForRowAtIndexPath:", &[table, path]);
        if cell == 0 {
            continue;
        }
        set_frame(cell, Rect::new(0.0, y, width, height));
        layout_cell(cell);
        set_cell_selected(cell, selection.iter().any(|&p| controls::same_path(p, path)));
        tell(delegate, "tableView:willDisplayCell:forRowAtIndexPath:", &[table, cell, path]);
        runtime::send(table, "addSubview:", &[cell]);
        shown.push((cell, path));
    }
    ROWS.lock().unwrap().get_or_insert_with(HashMap::new).insert(table, shown);
}

/// The rows are made now if they are due (a table asked about its cells
/// before its next layout, as UIKit's -_updateVisibleCellsNow does).
/// Whether they were made.
pub fn ensure_rows(table: Id) -> bool {
    let due = (!flag(table, "rowsLoaded", false) || flag(table, "needsReload", false)) && get_id(table, "dataSource") != 0;
    if due {
        load_rows(table);
    }
    due
}

/// UITableView's -reloadData: the rows are made again at the next layout
/// (or when the game asks for cells first).
extern "C" fn table_reload(this: Id, _: Sel) {
    controls::clear_selection(this);
    property_set(this, "needsReload", 1);
    runtime::send(this, "setNeedsLayout", &[]);
}

/// A table lays itself out: rows due are made, and as it scrolls the rows
/// coming into view get cells.
extern "C" fn table_layout_subviews(this: Id, _: Sel) {
    if (!flag(this, "rowsLoaded", false) || flag(this, "needsReload", false)) && get_id(this, "dataSource") != 0 {
        load_rows(this);
    } else {
        show_rows(this);
    }
}

extern "C" fn table_set_data_source(this: Id, _: Sel, source: Id) {
    property_set(this, "dataSource", source as u64);
    property_set(this, "rowsLoaded", 0);
}

extern "C" fn table_cell_for_path(this: Id, _: Sel, path: Id) -> Id {
    ensure_rows(this);
    rows_of(this).into_iter().find(|&(_, p)| controls::same_path(p, path)).map_or(0, |(cell, _)| cell)
}

extern "C" fn table_path_for_cell(this: Id, _: Sel, cell: Id) -> Id {
    ensure_rows(this);
    rows_of(this).into_iter().find(|&(c, _)| c == cell).map_or(0, |(_, path)| path)
}

extern "C" fn table_visible_cells(this: Id, _: Sel) -> Id {
    ensure_rows(this);
    let cells: Vec<Id> = rows_of(this).into_iter().map(|(cell, _)| cell).collect();
    collections::array_from(&cells)
}

extern "C" fn table_visible_paths(this: Id, _: Sel) -> Id {
    ensure_rows(this);
    let paths: Vec<Id> = rows_of(this).into_iter().map(|(_, path)| path).collect();
    collections::array_from(&paths)
}

/// The shown cells follow the table's selection.
fn show_selection(table: Id) {
    let selection = controls::selected(table);
    for (cell, path) in rows_of(table) {
        set_cell_selected(cell, selection.iter().any(|&p| controls::same_path(p, path)));
    }
}

extern "C" fn table_select(this: Id, _: Sel, path: Id, _animated: u8, _position: isize) {
    controls::select_path(this, path);
    show_selection(this);
}

extern "C" fn table_deselect(this: Id, _: Sel, path: Id, _animated: u8) {
    controls::deselect_path(this, path);
    show_selection(this);
}

/// A tap on a row: willSelect may pick another row (or none), the row
/// selected before is deselected (willDeselect/didDeselect), then didSelect.
fn tap_row(table: Id, path: Id) {
    if !flag(table, "allowsSelection", true) {
        return;
    }
    let delegate = get_id(table, "delegate");
    let path = match tell(delegate, "tableView:willSelectRowAtIndexPath:", &[table, path]) {
        Some(chosen) => chosen as Id,
        None => path,
    };
    if path == 0 {
        return;
    }
    if !flag(table, "allowsMultipleSelection", false) {
        for old in controls::selected(table).into_iter().filter(|&old| !controls::same_path(old, path)) {
            let old = match tell(delegate, "tableView:willDeselectRowAtIndexPath:", &[table, old]) {
                Some(chosen) => chosen as Id,
                None => old,
            };
            if old != 0 {
                controls::deselect_path(table, old);
                show_selection(table);
                tell(delegate, "tableView:didDeselectRowAtIndexPath:", &[table, old]);
            }
        }
    }
    controls::select_path(table, path);
    show_selection(table);
    tell(delegate, "tableView:didSelectRowAtIndexPath:", &[table, path]);
}

// ----- cells -----

/// UIKit's margin left and right of a cell's labels.
const CELL_MARGIN: f64 = 10.0;

fn line_height(label: Id) -> f64 {
    let (_, size) = controls::font_name_and_size(runtime::send(label, "font", &[]));
    text_size("X", size, f64::INFINITY, 1).height
}

fn text_width(label: Id) -> f64 {
    let text = get_id(label, "text");
    let (_, size) = controls::font_name_and_size(runtime::send(label, "font", &[]));
    if text == 0 { 0.0 } else { text_size(&string::get(text), size, f64::INFINITY, 1).width }
}

/// Places a cell's content view and its labels and picture by the cell's
/// style (default 0, value1 1, value2 2, subtitle 3), as UIKit's
/// -layoutSubviews does.
pub fn layout_cell(cell: Id) {
    let size = frame_of(cell).size;
    let content = runtime::send(cell, "contentView", &[]);
    if content != 0 {
        set_frame(content, Rect::new(0.0, 0.0, size.width, size.height));
    }
    let mut left = CELL_MARGIN;
    let right = size.width - CELL_MARGIN;
    let image_view = get_id(cell, "imageView");
    if image_view != 0 && get_id(image_view, "image") != 0 {
        let picture = crate::fw::uikit::image_points(get_id(image_view, "image"));
        let (width, height) = (picture.width.min(size.height), picture.height.min(size.height));
        set_frame(image_view, Rect::new(left, ((size.height - height) / 2.0).round(), width, height));
        left += width + CELL_MARGIN;
    }
    let text = get_id(cell, "textLabel");
    let detail = get_id(cell, "detailTextLabel");
    let room = (right - left).max(0.0);
    match property_get(cell, "cellStyle").unwrap_or(0) {
        1 | 2 if text != 0 && detail != 0 => {
            let detail_width = text_width(detail).min(room);
            set_frame(detail, Rect::new(right - detail_width, 0.0, detail_width, size.height));
            set_frame(text, Rect::new(left, 0.0, (room - detail_width - CELL_MARGIN).max(0.0), size.height));
        }
        3 if text != 0 && detail != 0 => {
            let (text_height, detail_height) = (line_height(text), line_height(detail));
            let top = ((size.height - text_height - detail_height) / 2.0).max(0.0).round();
            set_frame(text, Rect::new(left, top, room, text_height));
            set_frame(detail, Rect::new(left, top + text_height, room, detail_height));
        }
        _ if text != 0 => set_frame(text, Rect::new(left, 0.0, room, size.height)),
        _ => {}
    }
}

extern "C" fn cell_layout_subviews(this: Id, _: Sel) {
    layout_cell(this);
}

/// iOS 6's blue for a selected row; grey for the grey style.
const SELECTED_BLUE: [f64; 4] = [0.02, 0.45, 0.93, 1.0];
const SELECTED_GREY: [f64; 4] = [0.85, 0.85, 0.85, 1.0];

/// The colour a selected (or pressed) cell is filled with, when it has no
/// selectedBackgroundView and its selection style shows one.
pub fn cell_fill(cell: Id) -> Option<[f64; 4]> {
    let shown = flag(cell, "selected", false) || flag(cell, "highlighted", false);
    if !shown || get_id(cell, "selectedBackgroundView") != 0 {
        return None;
    }
    match property_get(cell, "selectionStyle").unwrap_or(1) {
        0 => None,
        2 => Some(SELECTED_GREY),
        _ => Some(SELECTED_BLUE),
    }
}

/// Selected or pressed: the labels take their highlighted colours and the
/// selectedBackgroundView goes behind the content.
fn show_cell_state(cell: Id) {
    let on = flag(cell, "selected", false) || flag(cell, "highlighted", false);
    let lit = on && property_get(cell, "selectionStyle").unwrap_or(1) != 0;
    for key in ["textLabel", "detailTextLabel"] {
        let label = get_id(cell, key);
        if label != 0 {
            property_set(label, "highlighted", lit as u64);
        }
    }
    let background = get_id(cell, "selectedBackgroundView");
    if background != 0 {
        if lit {
            let size = frame_of(cell).size;
            set_frame(background, Rect::new(0.0, 0.0, size.width, size.height));
            runtime::send(cell, "insertSubview:atIndex:", &[background, 0]);
        } else if view_state(background).superview == cell {
            runtime::send(background, "removeFromSuperview", &[]);
        }
    }
}

fn set_cell_selected(cell: Id, selected: bool) {
    // Through the cell's own method: a game's cell class may show it its way.
    runtime::send(cell, "setSelected:animated:", &[selected as usize, 0]);
}

extern "C" fn cell_set_selected(this: Id, _: Sel, selected: u8) {
    property_set(this, "selected", (selected != 0) as u64);
    show_cell_state(this);
}

extern "C" fn cell_set_selected_animated(this: Id, _: Sel, selected: u8, _animated: u8) {
    runtime::send(this, "setSelected:", &[selected as usize]);
}

extern "C" fn cell_set_highlighted(this: Id, _: Sel, highlighted: u8) {
    property_set(this, "highlighted", (highlighted != 0) as u64);
    show_cell_state(this);
}

extern "C" fn cell_set_highlighted_animated(this: Id, _: Sel, highlighted: u8, _animated: u8) {
    runtime::send(this, "setHighlighted:", &[highlighted as usize]);
}

/// The table a cell is shown in, and the cell's row.
fn table_and_path(cell: Id) -> Option<(Id, Id)> {
    let mut table = view_state(cell).superview;
    while table != 0 && !is(table, "UITableView") {
        table = view_state(table).superview;
    }
    let path = rows_of(table).into_iter().find(|&(c, _)| c == cell)?.1;
    Some((table, path))
}

/// How far a finger may move on a row before it is no longer a tap.
const TAP_SLOP: f64 = 10.0;

fn finger_at(touches: Id) -> Point {
    let touch = runtime::send(touches, "anyObject", &[]);
    crate::fw::touch::location_in(touch, 0)
}

extern "C" fn cell_touches_began(this: Id, _: Sel, touches: Id, _event: Id) {
    let at = finger_at(touches);
    property_set(this, "tapStart.x", at.x.to_bits());
    property_set(this, "tapStart.y", at.y.to_bits());
    property_set(this, "tapping", 1);
    let allowed = table_and_path(this).is_some_and(|(table, _)| flag(table, "allowsSelection", true));
    if allowed {
        runtime::send(this, "setHighlighted:animated:", &[1, 0]);
    }
}

extern "C" fn cell_touches_moved(this: Id, _: Sel, touches: Id, _event: Id) {
    let at = finger_at(touches);
    let start = Point {
        x: property_get(this, "tapStart.x").map_or(at.x, f64::from_bits),
        y: property_get(this, "tapStart.y").map_or(at.y, f64::from_bits),
    };
    if flag(this, "tapping", false) && ((at.x - start.x).abs() > TAP_SLOP || (at.y - start.y).abs() > TAP_SLOP) {
        property_set(this, "tapping", 0);
        runtime::send(this, "setHighlighted:animated:", &[0, 0]);
    }
}

extern "C" fn cell_touches_ended(this: Id, _: Sel, _touches: Id, _event: Id) {
    let tapped = flag(this, "tapping", false);
    property_set(this, "tapping", 0);
    runtime::send(this, "setHighlighted:animated:", &[0, 0]);
    if tapped {
        if let Some((table, path)) = table_and_path(this) {
            tap_row(table, path);
        }
    }
}

extern "C" fn cell_touches_cancelled(this: Id, _: Sel, _touches: Id, _event: Id) {
    property_set(this, "tapping", 0);
    runtime::send(this, "setHighlighted:animated:", &[0, 0]);
}

extern "C" fn cell_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    let s = runtime::sel("initWithFrame:");
    let parent = runtime::class_named("UIView");
    let imp = lock().lookup(parent, s).expect("UIView initWithFrame:");
    let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
    f(this, s, frame)
}

pub fn register(_: &mut Registry) {
    // UIKit's own classes for a cell's labels and content view, which cell nibs name.
    crate::fw::object::define_class("UITableViewLabel", "UILabel", 8);
    crate::fw::object::define_class("UITableViewCellContentView", "UIView", 8);
    methods(
        runtime::class_named("UITableView"),
        &[
            ("registerNib:forCellReuseIdentifier:", table_register_nib as usize),
            ("dequeueReusableCellWithIdentifier:", table_dequeue as usize),
            ("dequeueReusableCellWithIdentifier:forIndexPath:", table_dequeue_for_path as usize),
            ("reloadData", table_reload as usize),
            ("layoutSubviews", table_layout_subviews as usize),
            ("setDataSource:", table_set_data_source as usize),
            ("cellForRowAtIndexPath:", table_cell_for_path as usize),
            ("indexPathForCell:", table_path_for_cell as usize),
            ("visibleCells", table_visible_cells as usize),
            ("indexPathsForVisibleRows", table_visible_paths as usize),
            ("selectRowAtIndexPath:animated:scrollPosition:", table_select as usize),
            ("deselectRowAtIndexPath:animated:", table_deselect as usize),
        ],
    );
    methods(
        runtime::class_named("UITableViewCell"),
        &[
            ("initWithFrame:", cell_init_with_frame as usize),
            ("layoutSubviews", cell_layout_subviews as usize),
            ("setSelected:", cell_set_selected as usize),
            ("setSelected:animated:", cell_set_selected_animated as usize),
            ("setHighlighted:", cell_set_highlighted as usize),
            ("setHighlighted:animated:", cell_set_highlighted_animated as usize),
            ("touchesBegan:withEvent:", cell_touches_began as usize),
            ("touchesMoved:withEvent:", cell_touches_moved as usize),
            ("touchesEnded:withEvent:", cell_touches_ended as usize),
            ("touchesCancelled:withEvent:", cell_touches_cancelled as usize),
        ],
    );
}
