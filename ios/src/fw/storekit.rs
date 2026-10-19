//! In-app purchases (StoreKit). There is no store here: payments can't be
//! made, product requests fail the way they fail on an offline iPhone, and
//! no transaction ever arrives, so games carry on without the store.

use crate::fw::collections;
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::fw::uikit::{property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

/// SKErrorUnknown: what StoreKit reports when it can't reach the store.
const SK_ERROR_UNKNOWN: isize = 0;

extern "C" fn default_queue(_: Class, _: Sel) -> Id {
    static QUEUE: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *QUEUE.get_or_init(|| runtime::alloc_instance(runtime::class_named("SKPaymentQueue")))
}

extern "C" fn class_no(_: Class, _: Sel) -> bool {
    false
}

extern "C" fn ignore(_this: Id, _: Sel, _value: Id) {}

extern "C" fn ignore_none(_this: Id, _: Sel) {}

extern "C" fn no_transactions(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

/// +paymentWithProduct: / +paymentWithProductIdentifier: on SKPayment or
/// SKMutablePayment: an object of the class asked.
extern "C" fn class_payment_with(class: Class, _: Sel, _product: Id) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn payment_quantity(this: Id, _: Sel) -> isize {
    property_get(this, "quantity").map_or(1, |q| q as isize)
}

extern "C" fn payment_set_quantity(this: Id, _: Sel, quantity: isize) {
    property_set(this, "quantity", quantity as u64);
}

extern "C" fn request_init(this: Id, _: Sel, _identifiers: Id) -> Id {
    this
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

pub fn register(_r: &mut Registry) {
    let queue = define_class("SKPaymentQueue", "NSObject", 8);
    class_methods(queue, &[("defaultQueue", default_queue as usize), ("canMakePayments", class_no as usize)]);
    methods(
        queue,
        &[
            ("addTransactionObserver:", ignore as usize),
            ("removeTransactionObserver:", ignore as usize),
            ("addPayment:", ignore as usize),
            ("finishTransaction:", ignore as usize),
            ("restoreCompletedTransactions", ignore_none as usize),
            ("transactions", no_transactions as usize),
        ],
    );

    let payment = define_class("SKPayment", "NSObject", 8);
    class_methods(
        payment,
        &[
            ("paymentWithProduct:", class_payment_with as usize),
            ("paymentWithProductIdentifier:", class_payment_with as usize),
        ],
    );
    methods(payment, &[("quantity", payment_quantity as usize)]);
    let mutable = define_class("SKMutablePayment", "SKPayment", 8);
    methods(mutable, &[("setQuantity:", payment_set_quantity as usize)]);

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
    methods(products, &[("initWithProductIdentifiers:", request_init as usize)]);
}
