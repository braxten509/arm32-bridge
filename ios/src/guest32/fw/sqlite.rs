//! SQLite for 32-bit games, on the SQLite built into this layer.
//!
//! Databases and statements cross as handles. What SQLite hands back is
//! copied into guest memory: exec's rows (for the game's callback, for the
//! call) and error messages (the game frees them with sqlite3_free, which
//! frees guest memory), and column_text, which stays the same guest text
//! until the statement's next step, reset or finalize, as SQLite promises.
//! bind_text always lets SQLite take its own copy (SQLITE_TRANSIENT), so a
//! game destructor can be given its text back right away.

use std::collections::HashMap;
use std::sync::Mutex;

use arm32_bridge::handler;
use arm32_bridge::runtime::{call_guest, mem, stop_with_fault, ArgSource, Ret, Uc};
use libsqlite3_sys as sql;

use super::{forget_handle, handle_to_guest, handle_to_host};

const SQLITE_STATIC: u32 = 0;
const SQLITE_TRANSIENT: u32 = u32::MAX;

/// Guest copies of column_text results, by statement handle and column.
static TEXTS: Mutex<Option<HashMap<u32, HashMap<i32, u32>>>> = Mutex::new(None);

fn with_texts<R>(work: impl FnOnce(&mut HashMap<u32, HashMap<i32, u32>>) -> R) -> R {
    work(TEXTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// The statement moves on: its column texts are no longer valid.
fn forget_texts(statement: u32) {
    for (_, text) in with_texts(|texts| texts.remove(&statement)).unwrap_or_default() {
        crate::guest32::free(text);
    }
}

fn database(guest: u32) -> *mut sql::sqlite3 {
    handle_to_host(guest) as *mut sql::sqlite3
}

fn statement(guest: u32) -> *mut sql::sqlite3_stmt {
    handle_to_host(guest) as *mut sql::sqlite3_stmt
}

fn host_text(guest: u32) -> *const libc::c_char {
    mem().host_or_null(guest) as *const libc::c_char
}

/// A guest heap copy of host C string `text` (0 for null).
fn guest_copy(text: *const libc::c_char) -> u32 {
    if text.is_null() {
        return 0;
    }
    crate::guest32::cstring(&unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy())
}

/// What sqlite3_exec's row callback needs to call the game's.
struct RowCallback {
    uc: *mut Uc,
    function: u32,
    argument: u32,
    error: Option<String>,
}

/// One row for the game's callback(argument, count, values, names): the two
/// arrays and their texts in one guest block, for the call.
unsafe extern "C" fn row(context: *mut libc::c_void, count: libc::c_int, values: *mut *mut libc::c_char, names: *mut *mut libc::c_char) -> libc::c_int {
    let callback = &mut *(context as *mut RowCallback);
    let count = count.max(0) as u32;
    let lists = crate::guest32::alloc(count.max(1) * 8);
    let mut copies = Vec::new();
    for (list, host) in [(lists, values), (lists + count * 4, names)] {
        for i in 0..count {
            let copy = guest_copy(*host.add(i as usize));
            mem().write_u32(list + i * 4, copy);
            copies.push(copy);
        }
    }
    let result = call_guest(&mut *callback.uc, callback.function, &[callback.argument, count, lists, lists + count * 4]);
    for copy in copies.into_iter().filter(|&copy| copy != 0) {
        crate::guest32::free(copy);
    }
    crate::guest32::free(lists);
    match result {
        Ok(answer) => answer as u32 as i32,
        Err(error) => {
            callback.error = Some(error);
            1
        }
    }
}

pub fn register() {
    // sqlite3_open(filename, sqlite3 **): a handle even on failure (for sqlite3_errmsg).
    super::super::function(
        "sqlite3_open",
        handler!(|uc, a| {
            let (filename, out) = (a.u32(), a.u32());
            let mut db = std::ptr::null_mut();
            let result = unsafe { sql::sqlite3_open(host_text(filename), &mut db) };
            mem().write_u32(out, handle_to_guest(db as usize));
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_close",
        handler!(|uc, a| {
            let db = a.u32();
            let result = unsafe { sql::sqlite3_close(database(db)) };
            if result == sql::SQLITE_OK {
                forget_handle(db);
            }
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    // sqlite3_exec(db, sql, callback, argument, char **error)
    super::super::function(
        "sqlite3_exec",
        handler!(|uc, a| {
            let (db, text, function, argument, error_out) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let mut callback = RowCallback { uc: uc as *mut Uc, function, argument, error: None };
            let mut error = std::ptr::null_mut();
            let result = unsafe {
                sql::sqlite3_exec(
                    database(db),
                    host_text(text),
                    if function != 0 { Some(row) } else { None },
                    &mut callback as *mut RowCallback as *mut libc::c_void,
                    &mut error,
                )
            };
            if error_out != 0 {
                mem().write_u32(error_out, guest_copy(error));
            }
            unsafe { sql::sqlite3_free(error.cast()) };
            if let Some(error) = callback.error {
                stop_with_fault(uc, format!("sqlite3_exec's callback: {error}"));
                return Ret::Void;
            }
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    // Only guest memory reaches the game (see above), so this frees guest memory.
    super::super::function(
        "sqlite3_free",
        handler!(|uc, a| {
            let block = a.u32();
            if block != 0 {
                crate::guest32::free(block);
            }
            Ret::Void
        })
        .unwrap(),
    );
    // sqlite3_prepare_v2(db, sql, length, sqlite3_stmt **, const char **tail)
    super::super::function(
        "sqlite3_prepare_v2",
        handler!(|uc, a| {
            let (db, text, length, out, tail_out) = (a.u32(), a.u32(), a.i32(), a.u32(), a.u32());
            let (mut prepared, mut tail) = (std::ptr::null_mut(), std::ptr::null());
            let result = unsafe { sql::sqlite3_prepare_v2(database(db), host_text(text), length, &mut prepared, &mut tail) };
            if out != 0 {
                mem().write_u32(out, handle_to_guest(prepared as usize));
            }
            if tail_out != 0 {
                mem().write_u32(tail_out, if tail.is_null() { 0 } else { mem().guest(tail.cast()) });
            }
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_step",
        handler!(|uc, a| {
            let stmt = a.u32();
            forget_texts(stmt);
            Ret::U32(unsafe { sql::sqlite3_step(statement(stmt)) } as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_reset",
        handler!(|uc, a| {
            let stmt = a.u32();
            forget_texts(stmt);
            Ret::U32(unsafe { sql::sqlite3_reset(statement(stmt)) } as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_finalize",
        handler!(|uc, a| {
            let stmt = a.u32();
            forget_texts(stmt);
            let result = unsafe { sql::sqlite3_finalize(statement(stmt)) };
            forget_handle(stmt);
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_bind_int",
        handler!(|uc, a| {
            let (stmt, index, value) = (a.u32(), a.i32(), a.i32());
            Ret::U32(unsafe { sql::sqlite3_bind_int(statement(stmt), index, value) } as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_bind_double",
        handler!(|uc, a| {
            let (stmt, index, value) = (a.u32(), a.i32(), a.f64());
            Ret::U32(unsafe { sql::sqlite3_bind_double(statement(stmt), index, value) } as u32)
        })
        .unwrap(),
    );
    // sqlite3_bind_text(stmt, index, text, length, destructor)
    super::super::function(
        "sqlite3_bind_text",
        handler!(|uc, a| {
            let (stmt, index, text, length, destructor) = (a.u32(), a.i32(), a.u32(), a.i32(), a.u32());
            let result = unsafe { sql::sqlite3_bind_text(statement(stmt), index, host_text(text), length, sql::SQLITE_TRANSIENT()) };
            if text != 0 && destructor != SQLITE_STATIC && destructor != SQLITE_TRANSIENT {
                if let Err(error) = call_guest(uc, destructor, &[text]) {
                    stop_with_fault(uc, format!("sqlite3_bind_text's destructor: {error}"));
                    return Ret::Void;
                }
            }
            Ret::U32(result as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_column_int",
        handler!(|uc, a| {
            let (stmt, column) = (a.u32(), a.i32());
            Ret::U32(unsafe { sql::sqlite3_column_int(statement(stmt), column) } as u32)
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_column_double",
        handler!(|uc, a| {
            let (stmt, column) = (a.u32(), a.i32());
            Ret::F64(unsafe { sql::sqlite3_column_double(statement(stmt), column) })
        })
        .unwrap(),
    );
    super::super::function(
        "sqlite3_column_text",
        handler!(|uc, a| {
            let (stmt, column) = (a.u32(), a.i32());
            if let Some(text) = with_texts(|texts| texts.get(&stmt).and_then(|columns| columns.get(&column)).copied()) {
                return Ret::U32(text);
            }
            let host = statement(stmt);
            let text = unsafe { sql::sqlite3_column_text(host, column) };
            if text.is_null() {
                return Ret::U32(0);
            }
            let length = unsafe { sql::sqlite3_column_bytes(host, column) }.max(0) as usize;
            let copy = crate::guest32::alloc(length as u32 + 1);
            mem().write_bytes(copy, unsafe { std::slice::from_raw_parts(text, length) });
            mem().write_u8(copy + length as u32, 0);
            with_texts(|texts| texts.entry(stmt).or_default().insert(column, copy));
            Ret::U32(copy)
        })
        .unwrap(),
    );
}
