//! In-app purchases (StoreKit). There is no store here. What the owner
//! already bought (DLC) is listed per game in `<game home>/owned-purchases.txt`,
//! one product identifier per line (`#` starts a comment): product requests
//! answer those products (free, already paid for), a restore hands them all
//! back, and buying one again succeeds as a free re-download does on iOS.
//! Anything else can't be bought: it fails the way it fails on an offline
//! iPhone. Without the file nothing is owned, payments can't be made and
//! product requests fail, so games carry on without the store.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

/// SKErrorUnknown: what StoreKit reports when it can't reach the store.
const SK_ERROR_UNKNOWN: isize = 0;

// SKPaymentTransactionState
const PURCHASED: isize = 1;
const FAILED: isize = 2;
const RESTORED: isize = 3;

// ----- the owned list -----

static OWNED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Product identifiers, one per line; `#` starts a comment.
fn parse_owned(text: &str) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for line in text.lines() {
        let identifier = line.split('#').next().unwrap_or("").trim();
        if !identifier.is_empty() && !list.iter().any(|known| known == identifier) {
            list.push(identifier.to_string());
        }
    }
    list
}

/// What the owner bought, read once from the game's home folder.
fn owned() -> Vec<String> {
    OWNED
        .lock()
        .unwrap()
        .get_or_insert_with(|| {
            let path = format!("{}/owned-purchases.txt", crate::fw::foundation::home());
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let list = parse_owned(&text);
                    crate::log!("store: {} owned purchases in {path}: {}", list.len(), list.join(", "));
                    list
                }
                Err(_) => {
                    crate::log!("store: no {path}, so nothing is owned");
                    Vec::new()
                }
            }
        })
        .clone()
}

#[cfg(test)]
fn set_owned(list: Vec<String>) {
    *OWNED.lock().unwrap() = Some(list);
}

// ----- stored values: each getter answers the value kept under its name -----

static FIELDS: Mutex<Option<HashMap<(Id, String), Id>>> = Mutex::new(None);

fn field_set(object: Id, name: &str, value: Id) {
    FIELDS.lock().unwrap().get_or_insert_with(HashMap::new).insert((object, name.to_string()), value);
}

fn field(object: Id, name: &str) -> Id {
    FIELDS.lock().unwrap().as_ref().and_then(|fields| fields.get(&(object, name.to_string())).copied()).unwrap_or(0)
}

/// A getter: the value kept under the selector's name (nil when none).
extern "C" fn stored(this: Id, sel: Sel) -> Id {
    let name = lock().sel_name(sel);
    field(this, &name)
}

/// A setter (setFooBar:) keeping its value under fooBar; strings are copied.
extern "C" fn store_value(this: Id, sel: Sel, value: Id) {
    let name = lock().sel_name(sel);
    let name = name.strip_prefix("set").unwrap_or(&name).trim_end_matches(':');
    let mut letters = name.chars();
    let name = letters.next().map(|first| first.to_lowercase().chain(letters).collect::<String>()).unwrap_or_default();
    field_set(this, &name, copied(value));
}

fn copied(value: Id) -> Id {
    if value != 0 && string::is_string(value) { string::make(&string::get(value)) } else { value }
}

fn object_with(class: &str, values: &[(&str, Id)]) -> Id {
    let object = runtime::alloc_instance(runtime::class_named(class));
    for &(name, value) in values {
        field_set(object, name, value);
    }
    object
}

extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}

fn string_list(array: Id) -> Vec<String> {
    collections::array_items(array).into_iter().map(string::get).collect()
}

// ----- products -----

fn product(identifier: &str) -> Id {
    let name = string::make(identifier);
    let locale = runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]);
    object_with(
        "SKProduct",
        &[
            ("productIdentifier", name),
            ("localizedTitle", name),
            ("localizedDescription", name),
            ("price", collections::decimal_number(0)),
            ("priceLocale", locale),
            ("downloadContentLengths", collections::array_from(&[])),
            ("downloadContentVersion", string::make("")),
            ("contentVersion", string::make("")),
            ("discounts", collections::array_from(&[])),
        ],
    )
}

// ----- transactions and the payment queue -----

fn transaction_number() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{seconds}{:05}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// The same number every time for the first purchase of a product.
fn original_number(identifier: &str) -> String {
    let hash = identifier.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("1{:014}", hash % 100_000_000_000_000)
}

fn payment_for(identifier: &str) -> Id {
    object_with("SKPayment", &[("productIdentifier", string::make(identifier))])
}

/// A transaction for `payment`'s product: purchased or restored ones carry
/// the original purchase and an empty receipt; failed ones the error.
/// `downloads` stays nil (no hosted content), as Apple answers.
fn transaction(identifier: &str, state: isize, payment: Id) -> Id {
    let now = crate::fw::misc::now_since_2001();
    let mut values = vec![
        ("payment", payment),
        ("transactionState", state as Id),
        ("transactionDate", crate::fw::misc::date(now)),
    ];
    if state == FAILED {
        let error = crate::fw::misc::error("SKErrorDomain", SK_ERROR_UNKNOWN, "Cannot connect to iTunes Store");
        values.push(("error", error));
    } else {
        let original = object_with(
            "SKPaymentTransaction",
            &[
                ("payment", payment_for(identifier)),
                ("transactionState", PURCHASED as Id),
                ("transactionIdentifier", string::make(&original_number(identifier))),
                ("transactionDate", crate::fw::misc::date(now)),
                ("transactionReceipt", crate::fw::data::make(&[])),
            ],
        );
        values.extend([
            ("transactionIdentifier", string::make(&transaction_number())),
            ("transactionReceipt", crate::fw::data::make(&[])),
            ("originalTransaction", original),
        ]);
    }
    object_with("SKPaymentTransaction", &values)
}

#[derive(Default)]
struct Queue {
    observers: Vec<Id>,
    /// Transactions the game hasn't finished yet.
    unfinished: Vec<Id>,
    /// (observer, transaction) pairs already told; a transaction never
    /// changes here, so each observer hears it once.
    told: Vec<(Id, Id)>,
    /// A restore whose end no observer has heard yet.
    restore_pending: bool,
}

static QUEUES: Mutex<Option<HashMap<Id, Queue>>> = Mutex::new(None);

fn with_queue<T>(queue: Id, f: impl FnOnce(&mut Queue) -> T) -> T {
    f(QUEUES.lock().unwrap().get_or_insert_with(HashMap::new).entry(queue).or_default())
}

fn listening(queue: Id, observer: Id) -> bool {
    with_queue(queue, |q| q.observers.contains(&observer))
}

/// Brings the queue's observers up to date on the main run loop, as
/// StoreKit does: each hears the transactions still unfinished at its turn
/// that it hasn't heard yet, then the end of a waiting restore. One that
/// stops listening meanwhile hears nothing more; a restore's end waits
/// until some observer has heard it.
fn tell_later(queue: Id) {
    post(Work::Native(Box::new(move || {
        let observers = with_queue(queue, |q| q.observers.clone());
        let mut restore_heard = false;
        for observer in observers {
            let (news, restore_pending) = with_queue(queue, |q| {
                if !q.observers.contains(&observer) {
                    return (Vec::new(), false);
                }
                let news: Vec<Id> = q.unfinished.iter().copied().filter(|&t| !q.told.contains(&(observer, t))).collect();
                q.told.extend(news.iter().map(|&t| (observer, t)));
                (news, q.restore_pending)
            });
            let updated = "paymentQueue:updatedTransactions:";
            if !news.is_empty() && runtime::responds(observer, updated) {
                runtime::send(observer, updated, &[queue, collections::array_from(&news)]);
            }
            let finished = "paymentQueueRestoreCompletedTransactionsFinished:";
            if (restore_pending || restore_heard) && listening(queue, observer) && runtime::responds(observer, finished) {
                with_queue(queue, |q| q.restore_pending = false);
                restore_heard = true;
                runtime::send(observer, finished, &[queue]);
            }
        }
    })));
}

/// paymentQueue:removedTransactions: for the observers still listening.
fn tell_removed_later(queue: Id, transaction: Id) {
    post(Work::Native(Box::new(move || {
        let removed = "paymentQueue:removedTransactions:";
        for observer in with_queue(queue, |q| q.observers.clone()) {
            if listening(queue, observer) && runtime::responds(observer, removed) {
                runtime::send(observer, removed, &[queue, collections::array_from(&[transaction])]);
            }
        }
    })));
}

extern "C" fn default_queue(_: Class, _: Sel) -> Id {
    static QUEUE: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *QUEUE.get_or_init(|| runtime::alloc_instance(runtime::class_named("SKPaymentQueue")))
}

/// +canMakePayments: only when something is owned (nothing else can be bought).
extern "C" fn can_make_payments(_: Class, _: Sel) -> bool {
    !owned().is_empty()
}

/// -addTransactionObserver: added once; it hears the transactions waiting
/// (and a restore that ended with nobody listening).
extern "C" fn add_observer(this: Id, _: Sel, observer: Id) {
    let added = observer != 0
        && with_queue(this, |q| {
            let new = !q.observers.contains(&observer);
            if new {
                q.observers.push(observer);
            }
            new
        });
    if added {
        tell_later(this);
    }
}

/// -removeTransactionObserver: it hears nothing more; added again, it hears
/// the waiting transactions again, as on iOS.
extern "C" fn remove_observer(this: Id, _: Sel, observer: Id) {
    with_queue(this, |q| {
        q.observers.retain(|&o| o != observer);
        q.told.retain(|&(o, _)| o != observer);
    });
}

/// -addPayment: an owned product is handed over again (purchased); nothing
/// else can be bought here (failed, as when the store can't be reached).
/// The payment is copied first, as StoreKit does, so a mutable one changed
/// afterwards changes nothing.
extern "C" fn add_payment(this: Id, _: Sel, payment: Id) {
    if payment == 0 {
        return;
    }
    let identifier = string::get(runtime::send(payment, "productIdentifier", &[]));
    let copy = payment_for(&identifier);
    property_set(copy, "quantity", runtime::send(payment, "quantity", &[]) as u64);
    field_set(copy, "applicationUsername", copied(runtime::send(payment, "applicationUsername", &[])));
    field_set(copy, "requestData", runtime::send(payment, "requestData", &[]));
    let state = if owned().contains(&identifier) {
        crate::log!("store: game buys {identifier}: owned, so it is handed over again");
        PURCHASED
    } else {
        crate::log!("store: game buys {identifier}: not owned, answered 'cannot connect'");
        FAILED
    };
    let made = transaction(&identifier, state, copy);
    with_queue(this, |q| q.unfinished.push(made));
    tell_later(this);
}

/// -restoreCompletedTransactions (also ...WithApplicationUsername:): every
/// owned product comes back restored, then the restore finishes.
extern "C" fn restore(this: Id, _: Sel) {
    let owned = owned();
    crate::log!("store: game restores purchases: {} owned ({})", owned.len(), owned.join(", "));
    let made: Vec<Id> = owned.iter().map(|identifier| transaction(identifier, RESTORED, payment_for(identifier))).collect();
    with_queue(this, |q| {
        q.unfinished.extend(&made);
        q.restore_pending = true;
    });
    tell_later(this);
}

extern "C" fn restore_for_user(this: Id, sel: Sel, _user: Id) {
    restore(this, sel)
}

extern "C" fn finish_transaction(this: Id, _: Sel, transaction: Id) {
    let was_there = with_queue(this, |q| {
        let before = q.unfinished.len();
        q.unfinished.retain(|&t| t != transaction);
        q.told.retain(|&(_, t)| t != transaction);
        q.unfinished.len() != before
    });
    if was_there {
        tell_removed_later(this, transaction);
    }
}

extern "C" fn transactions(this: Id, _: Sel) -> Id {
    collections::array_from(&with_queue(this, |q| q.unfinished.clone()))
}

extern "C" fn ignore(_this: Id, _: Sel, _value: Id) {}

/// +paymentWithProduct: / +paymentWithProductIdentifier: on SKPayment or
/// SKMutablePayment: an object of the class asked, for that product.
extern "C" fn class_payment_with_product(class: Class, _: Sel, product: Id) -> Id {
    let identifier = if product == 0 { 0 } else { runtime::send(product, "productIdentifier", &[]) };
    class_payment_with_identifier(class, runtime::sel("paymentWithProductIdentifier:"), identifier)
}

extern "C" fn class_payment_with_identifier(class: Class, _: Sel, identifier: Id) -> Id {
    let payment = runtime::alloc_instance(class);
    field_set(payment, "productIdentifier", copied(identifier));
    payment
}

extern "C" fn payment_quantity(this: Id, _: Sel) -> isize {
    property_get(this, "quantity").map_or(1, |q| q as isize)
}

extern "C" fn payment_set_quantity(this: Id, _: Sel, quantity: isize) {
    property_set(this, "quantity", quantity as u64);
}

// ----- requests -----

/// -initWithProductIdentifiers: keeps a copy of the identifiers asked about.
extern "C" fn request_init(this: Id, _: Sel, identifiers: Id) -> Id {
    let asked = if identifiers == 0 { Vec::new() } else { collections::array_items(runtime::send(identifiers, "allObjects", &[])) };
    let asked: Vec<Id> = asked.into_iter().map(copied).collect();
    field_set(this, "productIdentifiers", collections::array_from(&asked));
    this
}

/// SKProductsRequest -start: the delegate hears the owned products asked
/// about and the rest as invalid, then requestDidFinish:. Owning nothing,
/// it fails as before.
extern "C" fn products_request_start(this: Id, sel: Sel) {
    let asked = string_list(field(this, "productIdentifiers"));
    crate::log!("store: game asked about products: {}", asked.join(", "));
    let owned = owned();
    if owned.is_empty() {
        return request_start(this, sel);
    }
    let request = this;
    post(Work::Native(Box::new(move || {
        if property_get(request, "cancelled").is_some() {
            return;
        }
        let (have, missing): (Vec<String>, Vec<String>) = asked.into_iter().partition(|identifier| owned.contains(identifier));
        let products: Vec<Id> = have.iter().map(|identifier| product(identifier)).collect();
        let invalid: Vec<Id> = missing.iter().map(|identifier| string::make(identifier)).collect();
        let response = object_with(
            "SKProductsResponse",
            &[("products", collections::array_from(&products)), ("invalidProductIdentifiers", collections::array_from(&invalid))],
        );
        let delegate = property_get(request, "delegate").unwrap_or(0) as Id;
        if delegate != 0 && runtime::responds(delegate, "productsRequest:didReceiveResponse:") {
            runtime::send(delegate, "productsRequest:didReceiveResponse:", &[request, response]);
        }
        // Read again: the game may let go of (or replace) it while answering.
        let delegate = property_get(request, "delegate").unwrap_or(0) as Id;
        if delegate != 0 && runtime::responds(delegate, "requestDidFinish:") {
            runtime::send(delegate, "requestDidFinish:", &[request]);
        }
    })));
}

extern "C" fn request_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
}

extern "C" fn request_delegate(this: Id, _: Sel) -> Id {
    property_get(this, "delegate").unwrap_or(0) as Id
}

/// -start: the delegate hears request:didFailWithError: on the main run loop.
extern "C" fn request_start(this: Id, _: Sel) {
    crate::log!("store: product request answered 'not available'");
    let block = crate::fw::media::make_callback(this as usize, |request| {
        let request = request as Id;
        if property_get(request, "cancelled").is_some() {
            return;
        }
        let delegate = property_get(request, "delegate").unwrap_or(0) as Id;
        if delegate != 0 && runtime::responds(delegate, "request:didFailWithError:") {
            let error = crate::fw::misc::error("SKErrorDomain", SK_ERROR_UNKNOWN, "Cannot connect to iTunes Store");
            runtime::send(delegate, "request:didFailWithError:", &[request as usize, error as usize]);
        }
    });
    post(Work::Block(block));
}

extern "C" fn request_cancel(this: Id, _: Sel) {
    property_set(this, "cancelled", 1);
}

extern "C" fn receipt_request_init(this: Id, _: Sel, properties: Id) -> Id {
    property_set(this, "receiptProperties", properties as u64);
    this
}

extern "C" fn receipt_properties(this: Id, _: Sel) -> Id {
    property_get(this, "receiptProperties").unwrap_or(0) as Id
}

/// SKStoreProductViewController: the product page can't load (no store);
/// the completion block hears NO with the error, and once shown the
/// controller says it finished, as when the user taps Done.
extern "C" fn product_load(_this: Id, _: Sel, _parameters: Id, completion: usize) {
    crate::log!("store: product page asked for; answering 'not available'");
    let error = crate::fw::misc::error("SKErrorDomain", SK_ERROR_UNKNOWN, "Cannot connect to iTunes Store");
    crate::fw::controls::call_block_later(completion, [0, error, 0]);
}

extern "C" fn product_answer_shown(this: Id, _: Sel) {
    crate::log!("store page shown: not available here, telling the game it was closed");
    let delegate = property_get(this, "delegate").unwrap_or(0) as Id;
    crate::fw::controls::tell(delegate, "productViewControllerDidFinish:", &[this]);
}

pub fn register(r: &mut Registry) {
    for (name, value) in [
        ("SKStoreProductParameterITunesItemIdentifier", "id"),
        ("SKStoreProductParameterAffiliateToken", "at"),
        ("SKStoreProductParameterCampaignToken", "ct"),
        ("SKStoreProductParameterProviderToken", "pt"),
        ("SKErrorDomain", "SKErrorDomain"),
    ] {
        r.data(name, crate::symbols::leak(crate::fw::string::make(value)));
    }
    let product_screen = define_class("SKStoreProductViewController", "UIViewController", 8);
    methods(
        product_screen,
        &[
            ("delegate", request_delegate as usize),
            ("setDelegate:", request_set_delegate as usize),
            ("loadProductWithParameters:completionBlock:", product_load as usize),
            ("gamebridgeAnswerShown", product_answer_shown as usize),
        ],
    );

    let queue = define_class("SKPaymentQueue", "NSObject", 8);
    class_methods(queue, &[("defaultQueue", default_queue as usize), ("canMakePayments", can_make_payments as usize)]);
    methods(
        queue,
        &[
            ("addTransactionObserver:", add_observer as usize),
            ("removeTransactionObserver:", remove_observer as usize),
            ("addPayment:", add_payment as usize),
            ("finishTransaction:", finish_transaction as usize),
            ("restoreCompletedTransactions", restore as usize),
            ("restoreCompletedTransactionsWithApplicationUsername:", restore_for_user as usize),
            ("transactions", transactions as usize),
            // Owned products have nothing to download.
            ("startDownloads:", ignore as usize),
            ("pauseDownloads:", ignore as usize),
            ("resumeDownloads:", ignore as usize),
            ("cancelDownloads:", ignore as usize),
        ],
    );

    let payment = define_class("SKPayment", "NSObject", 8);
    class_methods(
        payment,
        &[
            ("paymentWithProduct:", class_payment_with_product as usize),
            ("paymentWithProductIdentifier:", class_payment_with_identifier as usize),
        ],
    );
    methods(
        payment,
        &[
            ("quantity", payment_quantity as usize),
            ("productIdentifier", stored as usize),
            ("applicationUsername", stored as usize),
            ("requestData", stored as usize),
            ("simulatesAskToBuyInSandbox", no as usize),
        ],
    );
    let mutable = define_class("SKMutablePayment", "SKPayment", 8);
    methods(
        mutable,
        &[
            ("setQuantity:", payment_set_quantity as usize),
            ("setProductIdentifier:", store_value as usize),
            ("setApplicationUsername:", store_value as usize),
            ("setRequestData:", store_value as usize),
            ("setSimulatesAskToBuyInSandbox:", ignore as usize),
        ],
    );

    let product = define_class("SKProduct", "NSObject", 8);
    let getters = [
        "productIdentifier",
        "localizedTitle",
        "localizedDescription",
        "price",
        "priceLocale",
        "downloadContentLengths",
        "downloadContentVersion",
        "contentVersion",
        "discounts",
        "subscriptionPeriod",
        "introductoryPrice",
        "subscriptionGroupIdentifier",
    ];
    methods(product, &getters.map(|name| (name, stored as usize)));
    methods(product, &[("isDownloadable", no as usize), ("downloadable", no as usize), ("isFamilyShareable", no as usize)]);
    let response = define_class("SKProductsResponse", "NSObject", 8);
    methods(response, &[("products", stored as usize), ("invalidProductIdentifiers", stored as usize)]);
    let transaction = define_class("SKPaymentTransaction", "NSObject", 8);
    let getters = [
        "payment",
        "transactionState",
        "transactionIdentifier",
        "transactionDate",
        "transactionReceipt",
        "originalTransaction",
        "error",
        "downloads",
    ];
    methods(transaction, &getters.map(|name| (name, stored as usize)));

    let request = define_class("SKRequest", "NSObject", 8);
    methods(
        request,
        &[
            ("delegate", request_delegate as usize),
            ("setDelegate:", request_set_delegate as usize),
            ("start", request_start as usize),
            ("cancel", request_cancel as usize),
        ],
    );
    let products = define_class("SKProductsRequest", "SKRequest", 8);
    methods(products, &[("initWithProductIdentifiers:", request_init as usize), ("start", products_request_start as usize)]);
    // A receipt refresh fails like a product request (start is SKRequest's).
    let receipt = define_class("SKReceiptRefreshRequest", "SKRequest", 8);
    methods(
        receipt,
        &[("initWithReceiptProperties:", receipt_request_init as usize), ("receiptProperties", receipt_properties as usize)],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::string;
    use std::sync::Mutex;

    /// What the test observer and delegate heard: (who, message, argument).
    static HEARD: Mutex<Vec<(Id, &'static str, Id)>> = Mutex::new(Vec::new());
    /// The tests share the main run loop and the owned list: one at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    extern "C" fn heard_updated(this: Id, _: Sel, _queue: Id, transactions: Id) {
        HEARD.lock().unwrap().push((this, "updated", transactions));
    }
    extern "C" fn heard_restore_finished(this: Id, _: Sel, _queue: Id) {
        HEARD.lock().unwrap().push((this, "restore finished", 0));
    }
    extern "C" fn heard_response(this: Id, _: Sel, _request: Id, response: Id) {
        HEARD.lock().unwrap().push((this, "response", response));
    }
    extern "C" fn heard_finish(this: Id, _: Sel, _request: Id) {
        HEARD.lock().unwrap().push((this, "finished", 0));
    }
    extern "C" fn heard_failure(this: Id, _: Sel, _request: Id, error: Id) {
        HEARD.lock().unwrap().push((this, "failed", error));
    }

    /// The rude one finishes what it hears and stops listening at once.
    extern "C" fn rude_updated(this: Id, _: Sel, queue: Id, transactions: Id) {
        HEARD.lock().unwrap().push((this, "updated", transactions));
        for transaction in collections::array_items(transactions) {
            runtime::send(queue, "finishTransaction:", &[transaction]);
        }
        runtime::send(queue, "removeTransactionObserver:", &[this]);
    }
    /// ...and lets go of a request as soon as it answers.
    extern "C" fn rude_response(this: Id, _: Sel, request: Id, response: Id) {
        HEARD.lock().unwrap().push((this, "response", response));
        runtime::send(request, "setDelegate:", &[0]);
    }

    /// Sets up the layer, owns two products, and gives a new listener.
    fn start() -> (std::sync::MutexGuard<'static, ()>, Id) {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let listener = define_class("StoreTestListener", "NSObject", 8);
            methods(
                listener,
                &[
                    ("paymentQueue:updatedTransactions:", heard_updated as usize),
                    ("paymentQueueRestoreCompletedTransactionsFinished:", heard_restore_finished as usize),
                    ("productsRequest:didReceiveResponse:", heard_response as usize),
                    ("requestDidFinish:", heard_finish as usize),
                    ("request:didFailWithError:", heard_failure as usize),
                ],
            );
            let rude = define_class("StoreTestRude", "NSObject", 8);
            methods(
                rude,
                &[
                    ("paymentQueue:updatedTransactions:", rude_updated as usize),
                    ("paymentQueueRestoreCompletedTransactionsFinished:", heard_restore_finished as usize),
                    ("productsRequest:didReceiveResponse:", rude_response as usize),
                    ("requestDidFinish:", heard_finish as usize),
                ],
            );
        });
        set_owned(parse_owned("# bought on the old iPad\ncom.game.dlc1\n\n  com.game.dlc2  # map pack\n"));
        (guard, runtime::alloc_instance(runtime::class_named("StoreTestListener")))
    }

    /// Runs what is waiting on the main run loop, then what `who` heard.
    /// For a moment, not one turn: other tests run the same main loop at the
    /// same time and may be the ones to pick up an answer.
    fn heard_by(who: Id) -> Vec<(&'static str, Id)> {
        crate::fw::runloop::run_main_loop(Some(crate::fw::runloop::media_time() + 0.1));
        HEARD.lock().unwrap().iter().filter(|h| h.0 == who).map(|h| (h.1, h.2)).collect()
    }

    fn strings(array: Id) -> Vec<String> {
        collections::array_items(array).into_iter().map(string::get).collect()
    }

    fn product_of(transaction: Id) -> String {
        string::get(runtime::send(runtime::send(transaction, "payment", &[]), "productIdentifier", &[]))
    }

    #[test]
    fn products_request_answers_owned_products_and_lists_the_rest() {
        let (_guard, delegate) = start();
        let asked = crate::fw::misc::set_from(&[string::make("com.game.dlc1"), string::make("com.game.coins")]);
        let request = runtime::send(runtime::alloc_instance(runtime::class_named("SKProductsRequest")), "initWithProductIdentifiers:", &[asked]);
        runtime::send(request, "setDelegate:", &[delegate]);
        runtime::send(request, "start", &[]);
        let heard = heard_by(delegate);
        assert_eq!(heard.iter().map(|h| h.0).collect::<Vec<_>>(), ["response", "finished"]);
        let response = heard[0].1;
        assert_eq!(strings(runtime::send(response, "invalidProductIdentifiers", &[])), ["com.game.coins"]);
        let products = collections::array_items(runtime::send(response, "products", &[]));
        assert_eq!(products.len(), 1);
        let product = products[0];
        assert_eq!(string::get(runtime::send(product, "productIdentifier", &[])), "com.game.dlc1");
        assert_eq!(string::get(runtime::send(product, "localizedTitle", &[])), "com.game.dlc1");
        let price = runtime::send(product, "price", &[]);
        assert!(collections::is_number(price) && collections::number_as_f64(price) == 0.0);
        assert_ne!(runtime::send(product, "priceLocale", &[]), 0);
        assert_eq!(runtime::send(product, "isDownloadable", &[]) & 0xFF, 0);

        // A delegate that lets go of the request while hearing the answer
        // isn't told it finished.
        let rude = runtime::alloc_instance(runtime::class_named("StoreTestRude"));
        let request = runtime::send(runtime::alloc_instance(runtime::class_named("SKProductsRequest")), "initWithProductIdentifiers:", &[asked]);
        runtime::send(request, "setDelegate:", &[rude]);
        runtime::send(request, "start", &[]);
        assert_eq!(heard_by(rude).iter().map(|h| h.0).collect::<Vec<_>>(), ["response"]);
    }

    #[test]
    fn observers_hear_each_transaction_once_while_they_listen() {
        let (_guard, listener) = start();
        let rude_class = runtime::class_named("StoreTestRude");
        // The first observer finishes what it hears and leaves; one added
        // (twice) before the news arrives hears it once while unfinished,
        // and nothing once finished (nor is it remembered as told).
        let queue = runtime::alloc_instance(runtime::class_named("SKPaymentQueue"));
        let rude = runtime::alloc_instance(rude_class);
        runtime::send(queue, "addTransactionObserver:", &[listener]);
        runtime::send(queue, "addTransactionObserver:", &[rude]);
        let payment = runtime::send(runtime::class_named("SKPayment"), "paymentWithProductIdentifier:", &[string::make("com.game.dlc1")]);
        runtime::send(queue, "addPayment:", &[payment]);
        runtime::send(queue, "addTransactionObserver:", &[listener]);
        assert_eq!(heard_by(listener).iter().map(|h| h.0).collect::<Vec<_>>(), ["updated"]);
        assert_eq!(heard_by(rude).iter().map(|h| h.0).collect::<Vec<_>>(), ["updated"]);
        let late = runtime::alloc_instance(runtime::class_named("StoreTestListener"));
        runtime::send(queue, "addTransactionObserver:", &[late]);
        let payment = runtime::send(runtime::class_named("SKPayment"), "paymentWithProductIdentifier:", &[string::make("com.game.dlc2")]);
        runtime::send(queue, "addPayment:", &[payment]);
        let rude = runtime::alloc_instance(rude_class);
        runtime::send(queue, "addTransactionObserver:", &[rude]);
        // Order now: listener, late, rude, after. Rude finishes it once the
        // two before it heard it; the one after it is too late.
        let after = runtime::alloc_instance(runtime::class_named("StoreTestListener"));
        runtime::send(queue, "addTransactionObserver:", &[after]);
        assert_eq!(heard_by(late).len(), 1);
        assert!(heard_by(after).is_empty(), "finished before its turn");
        assert!(collections::array_items(runtime::send(queue, "transactions", &[])).is_empty());
        assert!(with_queue(queue, |q| q.told.is_empty()), "nothing told is kept once finished");

        // One that leaves while hearing a restore doesn't hear it finish;
        // the observer that comes next does.
        let queue = runtime::alloc_instance(runtime::class_named("SKPaymentQueue"));
        let rude = runtime::alloc_instance(rude_class);
        runtime::send(queue, "addTransactionObserver:", &[rude]);
        runtime::send(queue, "restoreCompletedTransactions", &[]);
        assert_eq!(heard_by(rude).iter().map(|h| h.0).collect::<Vec<_>>(), ["updated"]);
        let next = runtime::alloc_instance(runtime::class_named("StoreTestListener"));
        runtime::send(queue, "addTransactionObserver:", &[next]);
        assert_eq!(heard_by(next).iter().map(|h| h.0).collect::<Vec<_>>(), ["restore finished"]);

        // A restore nobody heard goes to the first observer that comes.
        let queue = runtime::alloc_instance(runtime::class_named("SKPaymentQueue"));
        runtime::send(queue, "restoreCompletedTransactions", &[]);
        crate::fw::runloop::run_main_loop(Some(0.0));
        let late = runtime::alloc_instance(runtime::class_named("StoreTestListener"));
        runtime::send(queue, "addTransactionObserver:", &[late]);
        let heard = heard_by(late);
        assert_eq!(heard.iter().map(|h| h.0).collect::<Vec<_>>(), ["updated", "restore finished"]);
        assert_eq!(collections::array_items(heard[0].1).len(), 2);
    }

    #[test]
    fn restore_brings_back_every_owned_product() {
        let (_guard, observer) = start();
        let queue = runtime::alloc_instance(runtime::class_named("SKPaymentQueue"));
        runtime::send(queue, "addTransactionObserver:", &[observer]);
        assert_ne!(runtime::send(runtime::class_named("SKPaymentQueue"), "canMakePayments", &[]) & 0xFF, 0);
        runtime::send(queue, "restoreCompletedTransactions", &[]);
        let heard = heard_by(observer);
        assert_eq!(heard.iter().map(|h| h.0).collect::<Vec<_>>(), ["updated", "restore finished"]);
        let transactions = collections::array_items(heard[0].1);
        assert_eq!(transactions.iter().map(|&t| product_of(t)).collect::<Vec<_>>(), ["com.game.dlc1", "com.game.dlc2"]);
        for &transaction in &transactions {
            assert_eq!(runtime::send(transaction, "transactionState", &[]), 3, "restored");
            assert!(string::get(runtime::send(transaction, "transactionIdentifier", &[])).len() > 0);
            assert_ne!(runtime::send(transaction, "transactionDate", &[]), 0);
            let original = runtime::send(transaction, "originalTransaction", &[]);
            assert_eq!(product_of(original), product_of(transaction));
            let receipt = runtime::send(transaction, "transactionReceipt", &[]);
            assert_eq!(string::get(runtime::send(receipt, "base64EncodedStringWithOptions:", &[0])), "");
        }
        // Unfinished until the game finishes them.
        assert_eq!(collections::array_items(runtime::send(queue, "transactions", &[])).len(), 2);
        runtime::send(queue, "finishTransaction:", &[transactions[0]]);
        assert_eq!(collections::array_items(runtime::send(queue, "transactions", &[])), [transactions[1]]);
    }

    #[test]
    fn only_owned_products_can_be_bought_again() {
        let (_guard, observer) = start();
        let gone = runtime::alloc_instance(runtime::class_named("StoreTestListener"));
        let queue = runtime::alloc_instance(runtime::class_named("SKPaymentQueue"));
        runtime::send(queue, "addTransactionObserver:", &[observer]);
        runtime::send(queue, "addTransactionObserver:", &[gone]);
        runtime::send(queue, "removeTransactionObserver:", &[gone]);
        let coins = runtime::send(runtime::class_named("SKPayment"), "paymentWithProductIdentifier:", &[string::make("com.game.coins")]);
        runtime::send(queue, "addPayment:", &[coins]);
        // A mutable payment changed after it was added still buys what it named then.
        let changed = runtime::send(runtime::alloc_instance(runtime::class_named("SKMutablePayment")), "init", &[]);
        runtime::send(changed, "setProductIdentifier:", &[string::make("com.game.dlc2")]);
        runtime::send(queue, "addPayment:", &[changed]);
        runtime::send(changed, "setProductIdentifier:", &[string::make("com.game.coins")]);
        let heard = heard_by(observer);
        assert!(heard.iter().all(|h| h.0 == "updated"));
        let told: Vec<Id> = heard.iter().flat_map(|h| collections::array_items(h.1)).collect();
        assert_eq!(told.len(), 2);
        let coins = told[0];
        assert_eq!((product_of(coins).as_str(), runtime::send(coins, "transactionState", &[])), ("com.game.coins", 2), "failed");
        let error = runtime::send(coins, "error", &[]);
        assert_eq!(string::get(runtime::send(error, "domain", &[])), "SKErrorDomain");
        let map = told[1];
        assert_eq!((product_of(map).as_str(), runtime::send(map, "transactionState", &[])), ("com.game.dlc2", 1), "purchased");
        // No hosted content: nil, as Apple answers.
        assert_eq!(runtime::send(map, "downloads", &[]), 0);
        assert!(heard_by(gone).is_empty(), "a removed observer hears nothing");
        // Owning nothing: no payments, as before.
        set_owned(parse_owned("# nothing yet\n"));
        assert_eq!(runtime::send(runtime::class_named("SKPaymentQueue"), "canMakePayments", &[]) & 0xFF, 0);
    }
}
