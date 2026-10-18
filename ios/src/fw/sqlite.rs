//! SQLite (the small database library iPhone apps link from the system).
//! The same SQLite is built in here; its C functions have the same layout
//! on Apple and Linux/Android, except sqlite3_config, which takes extra
//! (variadic) arguments.

use crate::symbols::Registry;
use libsqlite3_sys as sql;

crate::libsystem::libc::variadic!(ios_sqlite3_config, 1, ios_sqlite3_config_impl);

/// sqlite3_config(option, ...): reads exactly the values each option takes
/// (Apple passes them on the stack, 8 bytes each) and passes them on.
#[no_mangle]
unsafe extern "C" fn ios_sqlite3_config_impl(option: i32, va: *const u64) -> i32 {
    let pointer = |i: usize| *va.add(i) as usize;
    let int = |i: usize| *va.add(i) as u32 as i32;
    let wide = |i: usize| *va.add(i) as i64;
    match option {
        sql::SQLITE_CONFIG_SINGLETHREAD | sql::SQLITE_CONFIG_MULTITHREAD | sql::SQLITE_CONFIG_SERIALIZED | sql::SQLITE_CONFIG_PCACHE => {
            sql::sqlite3_config(option)
        }
        sql::SQLITE_CONFIG_MALLOC
        | sql::SQLITE_CONFIG_GETMALLOC
        | sql::SQLITE_CONFIG_MUTEX
        | sql::SQLITE_CONFIG_GETMUTEX
        | sql::SQLITE_CONFIG_PCACHE2
        | sql::SQLITE_CONFIG_GETPCACHE2
        | sql::SQLITE_CONFIG_PCACHE_HDRSZ => sql::sqlite3_config(option, pointer(0)),
        sql::SQLITE_CONFIG_MEMSTATUS
        | sql::SQLITE_CONFIG_URI
        | sql::SQLITE_CONFIG_COVERING_INDEX_SCAN
        | sql::SQLITE_CONFIG_WIN32_HEAPSIZE
        | sql::SQLITE_CONFIG_PMASZ
        | sql::SQLITE_CONFIG_STMTJRNL_SPILL
        | sql::SQLITE_CONFIG_SMALL_MALLOC
        | sql::SQLITE_CONFIG_SORTERREF_SIZE => sql::sqlite3_config(option, int(0)),
        sql::SQLITE_CONFIG_LOOKASIDE => sql::sqlite3_config(option, int(0), int(1)),
        sql::SQLITE_CONFIG_PAGECACHE | sql::SQLITE_CONFIG_HEAP => sql::sqlite3_config(option, pointer(0), int(1), int(2)),
        sql::SQLITE_CONFIG_LOG | sql::SQLITE_CONFIG_SQLLOG => sql::sqlite3_config(option, pointer(0), pointer(1)),
        sql::SQLITE_CONFIG_MMAP_SIZE => sql::sqlite3_config(option, wide(0), wide(1)),
        sql::SQLITE_CONFIG_MEMDB_MAXSIZE => sql::sqlite3_config(option, wide(0)),
        _ => {
            crate::log!("sqlite3_config option {option}: not known, refused");
            sql::SQLITE_ERROR
        }
    }
}

/// Registers SQLite functions by address (declared here only to take it).
macro_rules! same {
    ($r:ident, $($name:ident),* $(,)?) => {
        extern "C" {
            $( fn $name(); )*
        }
        $( $r.function(stringify!($name), $name as usize); )*
    };
}

pub fn register(r: &mut Registry) {
    r.function("sqlite3_config", ios_sqlite3_config as usize);
    same!(
        r,
        sqlite3_initialize, sqlite3_shutdown, sqlite3_threadsafe, sqlite3_libversion, sqlite3_libversion_number,
        sqlite3_open, sqlite3_open_v2, sqlite3_open16, sqlite3_close, sqlite3_close_v2, sqlite3_exec,
        sqlite3_prepare, sqlite3_prepare_v2, sqlite3_prepare16_v2, sqlite3_step, sqlite3_reset, sqlite3_finalize,
        sqlite3_clear_bindings, sqlite3_bind_blob, sqlite3_bind_double, sqlite3_bind_int, sqlite3_bind_int64,
        sqlite3_bind_null, sqlite3_bind_text, sqlite3_bind_text16, sqlite3_bind_zeroblob,
        sqlite3_bind_parameter_count, sqlite3_bind_parameter_index, sqlite3_bind_parameter_name,
        sqlite3_column_count, sqlite3_column_name, sqlite3_column_type, sqlite3_column_blob, sqlite3_column_bytes,
        sqlite3_column_bytes16, sqlite3_column_double, sqlite3_column_int, sqlite3_column_int64,
        sqlite3_column_text, sqlite3_column_text16, sqlite3_column_decltype, sqlite3_data_count,
        sqlite3_last_insert_rowid, sqlite3_changes, sqlite3_total_changes, sqlite3_errmsg, sqlite3_errmsg16,
        sqlite3_errcode, sqlite3_extended_errcode, sqlite3_errstr, sqlite3_busy_timeout, sqlite3_free,
        sqlite3_malloc, sqlite3_realloc, sqlite3_get_table, sqlite3_free_table, sqlite3_get_autocommit,
        sqlite3_db_handle, sqlite3_sql, sqlite3_interrupt, sqlite3_complete, sqlite3_table_column_metadata,
        sqlite3_create_function, sqlite3_create_function_v2, sqlite3_value_int, sqlite3_value_int64,
        sqlite3_value_double, sqlite3_value_text, sqlite3_value_blob, sqlite3_value_bytes, sqlite3_value_type,
        sqlite3_result_int, sqlite3_result_int64, sqlite3_result_double, sqlite3_result_text,
        sqlite3_result_null, sqlite3_result_blob, sqlite3_result_error, sqlite3_user_data,
        sqlite3_enable_shared_cache, sqlite3_release_memory, sqlite3_memory_used, sqlite3_sleep
    );
}
