//! Log lines and the end-of-run summary.

use std::io::Write;
use std::sync::Mutex;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::report::line(&format!($($arg)*))
    };
}

static OUT: Mutex<()> = Mutex::new(());
static MILESTONE: Mutex<Option<(bool, String)>> = Mutex::new(None);

pub fn line(text: &str) {
    let _guard = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let tid = unsafe { libc::gettid() };
    let _ = writeln!(std::io::stderr(), "[ios {tid}] {text}");
}

pub fn reached_did_finish(in_game: bool, place: &str) {
    *MILESTONE.lock().unwrap() = Some((in_game, place.to_string()));
}

/// Prints the summary and ends the process.
pub fn finish(code: i32) -> ! {
    static ONCE: Mutex<bool> = Mutex::new(false);
    {
        let mut done = ONCE.lock().unwrap_or_else(|e| e.into_inner());
        if *done {
            unsafe { libc::_exit(code) };
        }
        *done = true;
    }
    let mut summary = String::new();
    if let Some(report) = crate::loader::LOAD_REPORT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        summary += &format!(
            "imports: {} functions and {} data found; {} functions, {} data, {} classes missing; {} weak imports left empty\n",
            report.functions_found,
            report.data_found,
            report.functions_missing.len(),
            report.data_missing.len(),
            report.classes_missing.len(),
            report.weak_left_empty.len()
        );
        if std::env::var_os("IOS_LIST_MISSING").is_some() {
            summary += &format!("missing functions: {}\n", report.functions_missing.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("missing data: {}\n", report.data_missing.iter().cloned().collect::<Vec<_>>().join(" "));
            summary += &format!("missing classes: {}\n", report.classes_missing.iter().cloned().collect::<Vec<_>>().join(" "));
        }
    }
    let hits = crate::trap::hits();
    if !hits.is_empty() {
        summary += "missing functions reached (calls):";
        for (name, count) in &hits {
            summary += &format!(" {name}({count})");
        }
        summary += "\n";
    }
    let methods = crate::objc::runtime::missing_methods();
    if !methods.is_empty() {
        summary += "missing methods reached (calls):";
        for (name, count) in &methods {
            summary += &format!(" {name}({count})");
        }
        summary += "\n";
    }
    match MILESTONE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some((true, place)) => summary += &format!("MILESTONE: the game's own didFinishLaunching code ran ({place})\n"),
        Some((false, place)) => summary += &format!("milestone not met: didFinishLaunching is not game code ({place})\n"),
        None => summary += "milestone not met: didFinishLaunching was not reached\n",
    }
    for text in summary.lines() {
        line(text);
    }
    let _ = std::io::stderr().flush();
    unsafe { libc::_exit(code) };
}
