//! NSIndexPath, NSAssertionHandler, NSURLCredential and CATransaction.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, runloop, string};
use crate::libsystem::mach::{block_copy, call_block, BlockHeader};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::cell::RefCell;

// ----- NSIndexPath: { isa, *mut Vec<usize> } -----

fn indexes(object: Id) -> &'static Vec<usize> {
    unsafe {
        let slot = (object + 8) as *mut *mut Vec<usize>;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::default());
        }
        &**slot
    }
}

fn index_path_in(class: Class, list: Vec<usize>) -> Id {
    let object = runtime::alloc_instance(class);
    set_indexes(object, list);
    object
}

fn set_indexes(object: Id, list: Vec<usize>) {
    unsafe { *((object + 8) as *mut *mut Vec<usize>) = Box::into_raw(Box::new(list)) };
}

fn index_path(list: Vec<usize>) -> Id {
    index_path_in(runtime::class_named("NSIndexPath"), list)
}

unsafe fn read_indexes(pointer: *const usize, length: usize) -> Vec<usize> {
    if pointer.is_null() {
        Vec::new()
    } else {
        std::slice::from_raw_parts(pointer, length).to_vec()
    }
}

extern "C" fn ip_with_index(class: Class, _: Sel, index: usize) -> Id {
    index_path_in(class, vec![index])
}

extern "C" fn ip_with_indexes(class: Class, _: Sel, pointer: *const usize, length: usize) -> Id {
    index_path_in(class, unsafe { read_indexes(pointer, length) })
}

extern "C" fn ip_for_row(class: Class, _: Sel, row: isize, section: isize) -> Id {
    index_path_in(class, vec![section as usize, row as usize])
}

extern "C" fn ip_init(this: Id, _: Sel) -> Id {
    set_indexes(this, Vec::new());
    this
}

extern "C" fn ip_init_with_index(this: Id, _: Sel, index: usize) -> Id {
    set_indexes(this, vec![index]);
    this
}

extern "C" fn ip_init_with_indexes(this: Id, _: Sel, pointer: *const usize, length: usize) -> Id {
    set_indexes(this, unsafe { read_indexes(pointer, length) });
    this
}

/// row/item is the second index, section the first; NSNotFound when missing.
fn position(this: Id, at: usize) -> isize {
    indexes(this).get(at).map(|&i| i as isize).unwrap_or(isize::MAX)
}

extern "C" fn ip_section(this: Id, _: Sel) -> isize {
    position(this, 0)
}

extern "C" fn ip_row(this: Id, _: Sel) -> isize {
    position(this, 1)
}

extern "C" fn ip_length(this: Id, _: Sel) -> usize {
    indexes(this).len()
}

extern "C" fn ip_index_at(this: Id, _: Sel, at: usize) -> usize {
    position(this, at) as usize
}

extern "C" fn ip_by_adding(this: Id, _: Sel, index: usize) -> Id {
    let mut list = indexes(this).clone();
    list.push(index);
    index_path(list)
}

extern "C" fn ip_by_removing_last(this: Id, _: Sel) -> Id {
    let mut list = indexes(this).clone();
    list.pop();
    index_path(list)
}

extern "C" fn ip_get_indexes(this: Id, _: Sel, out: *mut usize) {
    let list = indexes(this);
    if !out.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(list.as_ptr(), out, list.len()) };
    }
}

extern "C" fn ip_get_indexes_range(this: Id, _: Sel, out: *mut usize, range: string::Range) {
    let list = indexes(this);
    let end = (range.location + range.length).min(list.len());
    if !out.is_null() && range.location < end {
        unsafe { std::ptr::copy_nonoverlapping(list[range.location..end].as_ptr(), out, end - range.location) };
    }
}

/// Index by index, then the shorter path first (NSOrderedAscending = -1).
pub fn compare_paths(a: &[usize], b: &[usize]) -> isize {
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn is_index_path(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), rt.by_name.get("NSIndexPath").copied().unwrap_or(0))
}

extern "C" fn ip_compare(this: Id, _: Sel, other: Id) -> isize {
    compare_paths(indexes(this), if other == 0 { &[] } else { indexes(other) })
}

extern "C" fn ip_is_equal(this: Id, _: Sel, other: Id) -> bool {
    is_index_path(other) && indexes(this) == indexes(other)
}

extern "C" fn ip_hash(this: Id, _: Sel) -> usize {
    indexes(this).iter().fold(indexes(this).len(), |h, &i| h.wrapping_mul(31).wrapping_add(i))
}

extern "C" fn ip_copy(this: Id, _: Sel, _zone: usize) -> Id {
    this
}

extern "C" fn ip_description(this: Id, _: Sel) -> Id {
    let list: Vec<String> = indexes(this).iter().map(|i| i.to_string()).collect();
    string::make(&format!("<NSIndexPath: {this:#x}> {{length = {}, path = {}}}", list.len(), list.join(" - ")))
}

// ----- NSAssertionHandler -----
// A failed NSAssert on iOS raises NSInternalInconsistencyException; with no
// handler the app ends. Here the same message is logged and the run stops.

crate::libsystem::libc::variadic!(ios_assert_method, 7, ios_assert_method_impl);
crate::libsystem::libc::variadic!(ios_assert_function, 6, ios_assert_function_impl);

/// NSAssertionHandlerKey: the thread dictionary key of a thread's handler.
const HANDLER_KEY: &str = "NSAssertionHandler";

/// The handler in the current thread's dictionary under NSAssertionHandlerKey
/// (a game may put its own there); one is made and stored when there is none.
extern "C" fn assertion_current(class: Class, _: Sel) -> Id {
    let thread = runtime::send(runtime::class_named("NSThread"), "currentThread", &[]);
    let dictionary = if thread == 0 { 0 } else { runtime::send(thread, "threadDictionary", &[]) };
    let key = string::make(HANDLER_KEY);
    if dictionary != 0 {
        let handler = runtime::send(dictionary, "objectForKey:", &[key]);
        if handler != 0 {
            return handler;
        }
    }
    let handler = runtime::send(runtime::alloc_instance(class), "init", &[]);
    if dictionary != 0 {
        runtime::send(dictionary, "setObject:forKey:", &[handler, key]);
    }
    handler
}

fn assertion_failed(place: String, file: Id, line: isize, description: Id, va: *const u64) -> ! {
    let reason = if description == 0 { String::new() } else { string::format_object(description, va) };
    crate::log!("*** Assertion failure in {place}, {}:{line}", string::get(file));
    crate::log!("STOP: NSInternalInconsistencyException: {reason}");
    crate::report::finish(1);
}

#[no_mangle]
extern "C" fn ios_assert_method_impl(_this: Id, _: Sel, method: Sel, object: Id, file: Id, line: isize, description: Id, va: *const u64) {
    let (class_name, sign) = if object == 0 {
        ("nil".to_string(), '-')
    } else {
        let rt = lock();
        let class = rt.class_of(object);
        let meta = rt.classes.get(&class).map(|r| r.meta).unwrap_or(false);
        (rt.class_name(class), if meta { '+' } else { '-' })
    };
    let method = lock().sel_name(method);
    assertion_failed(format!("{sign}[{class_name} {method}]"), file, line, description, va);
}

#[no_mangle]
extern "C" fn ios_assert_function_impl(_this: Id, _: Sel, function: Id, file: Id, line: isize, description: Id, va: *const u64) {
    assertion_failed(string::get(function), file, line, description, va);
}

// ----- NSURLCredential: { isa, user, password, persistence, trust } -----

fn credential(class: Class, user: Id, password: Id, persistence: usize, trust: usize) -> Id {
    let object = runtime::alloc_instance(class);
    credential_fill(object, user, password, persistence, trust);
    object
}

fn credential_fill(object: Id, user: Id, password: Id, persistence: usize, trust: usize) {
    let copy = |s: Id| if s == 0 { 0 } else { runtime::send(s, "copy", &[]) };
    unsafe {
        let fields = (object + 8) as *mut [usize; 4];
        *fields = [copy(user), copy(password), persistence, trust];
    }
}

fn field(object: Id, index: usize) -> usize {
    unsafe { (*((object + 8) as *const [usize; 4]))[index] }
}

extern "C" fn credential_with_user(class: Class, _: Sel, user: Id, password: Id, persistence: usize) -> Id {
    credential(class, user, password, persistence, 0)
}

extern "C" fn credential_init_user(this: Id, _: Sel, user: Id, password: Id, persistence: usize) -> Id {
    credential_fill(this, user, password, persistence, 0);
    this
}

/// A server-trust credential: kept with the trust it was made from
/// (persistence "for session", as Apple sets it).
extern "C" fn credential_for_trust(class: Class, _: Sel, trust: usize) -> Id {
    credential(class, 0, 0, 1, trust)
}

extern "C" fn credential_init_trust(this: Id, _: Sel, trust: usize) -> Id {
    credential_fill(this, 0, 0, 1, trust);
    this
}

extern "C" fn credential_user(this: Id, _: Sel) -> Id {
    field(this, 0)
}

extern "C" fn credential_password(this: Id, _: Sel) -> Id {
    field(this, 1)
}

extern "C" fn credential_has_password(this: Id, _: Sel) -> bool {
    field(this, 1) != 0
}

extern "C" fn credential_persistence(this: Id, _: Sel) -> usize {
    field(this, 2)
}

extern "C" fn credential_certificates(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

extern "C" fn credential_identity(_this: Id, _: Sel) -> usize {
    0
}

// ----- CATransaction -----
// Nothing animates here, so a transaction only keeps its settings (nested,
// per thread, as on Apple) and runs its completion block once the outermost
// transaction commits, on the main run loop.

struct Transaction {
    /// Started by a setter outside begin/commit. Apple commits it at the end
    /// of the run loop turn; so does the main thread here (other threads at
    /// +flush).
    implicit: bool,
    disable_actions: Option<bool>,
    duration: Option<f64>,
    timing: Option<Id>,
    completion: usize,
    /// Completion blocks of nested transactions, run with this one's.
    pending: Vec<usize>,
    values: Id,
}

impl Transaction {
    fn new(implicit: bool) -> Self {
        Transaction { implicit, disable_actions: None, duration: None, timing: None, completion: 0, pending: Vec::new(), values: 0 }
    }

    fn blocks(self) -> Vec<usize> {
        let mut blocks = self.pending;
        if self.completion != 0 {
            blocks.push(self.completion);
        }
        blocks
    }
}

thread_local! {
    static TRANSACTIONS: RefCell<Vec<Transaction>> = const { RefCell::new(Vec::new()) };
}

const DEFAULT_DURATION: f64 = 0.25;

fn run_completions(blocks: Vec<usize>) {
    for block in blocks {
        runloop::post(runloop::Work::Native(Box::new(move || unsafe { call_block(block) })));
    }
}

/// The innermost transaction (an implicit one is started when there is none).
fn with_top<R>(f: impl FnOnce(&mut Transaction) -> R) -> R {
    let (result, started) = TRANSACTIONS.with(|t| {
        let mut list = t.borrow_mut();
        let started = list.is_empty();
        if started {
            list.push(Transaction::new(true));
        }
        (f(list.last_mut().unwrap()), started)
    });
    if started {
        commit_implicit_later();
    }
    result
}

/// On the main thread, the implicit transaction commits once the current
/// run loop turn is over.
fn commit_implicit_later() {
    if runloop::is_main_thread() {
        runloop::post(runloop::Work::Native(Box::new(|| ca_flush(0, 0))));
    }
}

/// A setting as seen now: the innermost transaction that set it wins.
fn lookup<R>(f: impl Fn(&Transaction) -> Option<R>) -> Option<R> {
    TRANSACTIONS.with(|t| t.borrow().iter().rev().find_map(f))
}

extern "C" fn ca_begin(_: Class, _: Sel) {
    TRANSACTIONS.with(|t| t.borrow_mut().push(Transaction::new(false)));
}

/// Nothing animates, so a transaction is done when committed; a nested one's
/// completion blocks still wait for the outer transaction, as on Apple.
extern "C" fn ca_commit(_: Class, _: Sel) {
    let ready = TRANSACTIONS.with(|t| {
        let mut list = t.borrow_mut();
        if list.last().map(|top| top.implicit).unwrap_or(true) {
            return None;
        }
        let finished = list.pop().unwrap().blocks();
        match list.last_mut() {
            Some(parent) => {
                parent.pending.extend(finished);
                if list.len() == 1 && list[0].implicit {
                    // The implicit one waited for this transaction.
                    commit_implicit_later();
                }
                Some(Vec::new())
            }
            None => Some(finished),
        }
    });
    match ready {
        Some(blocks) => run_completions(blocks),
        None => crate::log!("CATransaction commit without begin; ignored"),
    }
}

/// Commits the implicit transaction, when it is the only one open.
extern "C" fn ca_flush(_: Class, _: Sel) {
    let implicit = TRANSACTIONS.with(|t| {
        let mut list = t.borrow_mut();
        if list.len() == 1 && list[0].implicit { list.pop() } else { None }
    });
    if let Some(transaction) = implicit {
        run_completions(transaction.blocks());
    }
}

extern "C" fn ca_nothing(_: Class, _: Sel) {}

extern "C" fn ca_disable_actions(_: Class, _: Sel) -> bool {
    lookup(|t| t.disable_actions).unwrap_or(false)
}

extern "C" fn ca_set_disable_actions(_: Class, _: Sel, value: u8) {
    with_top(|t| t.disable_actions = Some(value != 0));
}

extern "C" fn ca_duration(_: Class, _: Sel) -> f64 {
    lookup(|t| t.duration).unwrap_or(DEFAULT_DURATION)
}

extern "C" fn ca_set_duration(_: Class, _: Sel, value: f64) {
    with_top(|t| t.duration = Some(value));
}

extern "C" fn ca_timing(_: Class, _: Sel) -> Id {
    lookup(|t| t.timing).unwrap_or(0)
}

extern "C" fn ca_set_timing(_: Class, _: Sel, value: Id) {
    with_top(|t| t.timing = Some(value));
}

extern "C" fn ca_completion(_: Class, _: Sel) -> usize {
    lookup(|t| (t.completion != 0).then_some(t.completion)).unwrap_or(0)
}

/// Outside any begin/commit the block belongs to the implicit transaction,
/// which runs it when it commits; as on Apple, setting another block (or nil)
/// replaces it.
extern "C" fn ca_set_completion(_: Class, _: Sel, block: usize) {
    let block = if block == 0 { 0 } else { block_copy(block as *mut BlockHeader) as usize };
    with_top(|t| t.completion = block);
}

extern "C" fn ca_value_for_key(class: Class, s: Sel, key: Id) -> Id {
    match string::get(key).as_str() {
        "disableActions" => collections::number_bool(ca_disable_actions(class, s)),
        "animationDuration" => collections::number_float(ca_duration(class, s)),
        "animationTimingFunction" => ca_timing(class, s),
        "completionBlock" => ca_completion(class, s),
        _ => lookup(|t| (t.values != 0).then(|| collections::dictionary_get(t.values, key)).filter(|&v| v != 0)).unwrap_or(0),
    }
}

extern "C" fn ca_set_value_for_key(class: Class, s: Sel, value: Id, key: Id) {
    match string::get(key).as_str() {
        "disableActions" => ca_set_disable_actions(class, s, (value != 0 && runtime::send(value, "boolValue", &[]) & 0xFF != 0) as u8),
        "animationDuration" => ca_set_duration(class, s, if value == 0 { 0.0 } else { collections::number_as_f64(value) }),
        "animationTimingFunction" => ca_set_timing(class, s, value),
        "completionBlock" => ca_set_completion(class, s, value),
        _ => with_top(|t| {
            if t.values == 0 {
                t.values = runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]);
            }
            if value == 0 {
                runtime::send(t.values, "removeObjectForKey:", &[key]);
            } else {
                runtime::send(t.values, "setObject:forKey:", &[value, key]);
            }
        }),
    }
}

pub fn register(r: &mut Registry) {
    r.data("NSAssertionHandlerKey", leak(string::make(HANDLER_KEY)));
    let index_path_class = define_class("NSIndexPath", "NSObject", 16);
    class_methods(
        index_path_class,
        &[
            ("indexPathWithIndex:", ip_with_index as usize),
            ("indexPathWithIndexes:length:", ip_with_indexes as usize),
            ("indexPathForRow:inSection:", ip_for_row as usize),
            ("indexPathForItem:inSection:", ip_for_row as usize),
        ],
    );
    methods(
        index_path_class,
        &[
            ("init", ip_init as usize),
            ("initWithIndex:", ip_init_with_index as usize),
            ("initWithIndexes:length:", ip_init_with_indexes as usize),
            ("section", ip_section as usize),
            ("row", ip_row as usize),
            ("item", ip_row as usize),
            ("length", ip_length as usize),
            ("indexAtPosition:", ip_index_at as usize),
            ("indexPathByAddingIndex:", ip_by_adding as usize),
            ("indexPathByRemovingLastIndex", ip_by_removing_last as usize),
            ("getIndexes:", ip_get_indexes as usize),
            ("getIndexes:range:", ip_get_indexes_range as usize),
            ("compare:", ip_compare as usize),
            ("isEqual:", ip_is_equal as usize),
            ("hash", ip_hash as usize),
            ("copyWithZone:", ip_copy as usize),
            ("description", ip_description as usize),
        ],
    );

    let handler = define_class("NSAssertionHandler", "NSObject", 8);
    class_methods(handler, &[("currentHandler", assertion_current as usize)]);
    methods(
        handler,
        &[
            ("handleFailureInMethod:object:file:lineNumber:description:", ios_assert_method as usize),
            ("handleFailureInFunction:file:lineNumber:description:", ios_assert_function as usize),
        ],
    );

    let credential_class = define_class("NSURLCredential", "NSObject", 40);
    class_methods(
        credential_class,
        &[
            ("credentialWithUser:password:persistence:", credential_with_user as usize),
            ("credentialForTrust:", credential_for_trust as usize),
        ],
    );
    methods(
        credential_class,
        &[
            ("initWithUser:password:persistence:", credential_init_user as usize),
            ("initWithTrust:", credential_init_trust as usize),
            ("user", credential_user as usize),
            ("password", credential_password as usize),
            ("hasPassword", credential_has_password as usize),
            ("persistence", credential_persistence as usize),
            ("certificates", credential_certificates as usize),
            ("identity", credential_identity as usize),
        ],
    );

    let transaction = define_class("CATransaction", "NSObject", 8);
    class_methods(
        transaction,
        &[
            ("begin", ca_begin as usize),
            ("commit", ca_commit as usize),
            ("flush", ca_flush as usize),
            ("lock", ca_nothing as usize),
            ("unlock", ca_nothing as usize),
            ("disableActions", ca_disable_actions as usize),
            ("setDisableActions:", ca_set_disable_actions as usize),
            ("animationDuration", ca_duration as usize),
            ("setAnimationDuration:", ca_set_duration as usize),
            ("animationTimingFunction", ca_timing as usize),
            ("setAnimationTimingFunction:", ca_set_timing as usize),
            ("completionBlock", ca_completion as usize),
            ("setCompletionBlock:", ca_set_completion as usize),
            ("valueForKey:", ca_value_for_key as usize),
            ("setValue:forKey:", ca_set_value_for_key as usize),
        ],
    );
}
