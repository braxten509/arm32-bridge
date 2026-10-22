//! NSCalendar, NSDateComponents and NSDateFormatter.
//! Date math uses the Gregorian calendar; the calendars built on it (ISO 8601,
//! Buddhist, Japanese, Republic of China) only name years and eras
//! differently, and Islamic civil has its own arithmetic. Other calendars are
//! computed as Gregorian (logged once). Times are in the phone's own time zone
//! unless the game sets another; names and patterns are English, as the
//! layer's NSLocale is.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, misc, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashSet;
use std::sync::Mutex;

const SINCE_1970: f64 = 978_307_200.0;
/// NSDateComponentUndefined (NSIntegerMax).
pub const UNDEFINED: i64 = i64::MAX;

// ----- Gregorian arithmetic (days since 1970-01-01) -----

pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 1 = Sunday ... 7 = Saturday.
pub fn weekday_of(days: i64) -> i64 {
    (days + 4).rem_euclid(7) + 1
}

/// Wall-clock seconds since 1970 for a time of day on a day.
fn wall_of(days: i64, hour: i64, minute: i64, second: i64, nanosecond: i64) -> f64 {
    (days * 86_400 + hour * 3600 + minute * 60 + second) as f64 + nanosecond as f64 / 1e9
}

/// (days, hour, minute, second, nanosecond) of a wall-clock time.
fn split_wall(wall: f64) -> (i64, i64, i64, i64, i64) {
    let mut whole = wall.floor() as i64;
    let mut nanos = ((wall - wall.floor()) * 1e9).round() as i64;
    if nanos >= 1_000_000_000 {
        whole += 1;
        nanos -= 1_000_000_000;
    }
    let rest = whole.rem_euclid(86_400);
    (whole.div_euclid(86_400), rest / 3600, rest / 60 % 60, rest % 60, nanos)
}

// ----- calendars -----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Gregorian,
    Iso8601,
    Buddhist,
    Japanese,
    RepublicOfChina,
    IslamicCivil,
    /// Chinese, Hebrew, Indian, Islamic (astronomical), Persian.
    Other,
}

pub fn kind_of(identifier: &str) -> Option<Kind> {
    Some(match identifier {
        "gregorian" => Kind::Gregorian,
        "iso8601" => Kind::Iso8601,
        "buddhist" => Kind::Buddhist,
        "japanese" => Kind::Japanese,
        "roc" => Kind::RepublicOfChina,
        "islamic-civil" | "islamic-tbla" => Kind::IslamicCivil,
        "chinese" | "hebrew" | "indian" | "islamic" | "islamic-umalqura" | "persian" | "coptic" | "ethiopic"
        | "ethiopic-amete-alem" => Kind::Other,
        _ => return None,
    })
}

/// Japanese eras: (Gregorian start year, month, day, Apple's era number, name).
const JAPANESE_ERAS: [(i64, i64, i64, i64, &str); 5] = [
    (1868, 9, 8, 232, "Meiji"),
    (1912, 7, 30, 233, "Taishō"),
    (1926, 12, 25, 234, "Shōwa"),
    (1989, 1, 8, 235, "Heisei"),
    (2019, 5, 1, 236, "Reiwa"),
];

fn islamic_to_jd(year: i64, month: i64, day: i64) -> i64 {
    day + (29.5 * (month - 1) as f64).ceil() as i64 + (year - 1) * 354 + (3 + 11 * year).div_euclid(30) + 1_948_439
}

fn islamic_from_days(days: i64) -> (i64, i64, i64) {
    let jd = days + 2_440_588;
    let year = (30 * (jd - 1_948_440) + 10_646).div_euclid(10_631);
    let month = (((jd - 29 - islamic_to_jd(year, 1, 1)) as f64 / 29.5).ceil() as i64 + 1).clamp(1, 12);
    (year, month, jd - islamic_to_jd(year, month, 1) + 1)
}

/// A date as a calendar names it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CalDate {
    pub era: i64,
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub nanosecond: i64,
    /// Days since 1970-01-01.
    pub days: i64,
    /// The Gregorian year (for day-of-year and week numbers).
    pub gregorian_year: i64,
}

pub fn cal_date(kind: Kind, wall: f64) -> CalDate {
    let (days, hour, minute, second, nanosecond) = split_wall(wall);
    let (gy, gm, gd) = civil_from_days(days);
    let (era, year, month, day) = match kind {
        Kind::Gregorian | Kind::Iso8601 | Kind::Other => {
            if gy > 0 {
                (1, gy, gm, gd)
            } else {
                (0, 1 - gy, gm, gd)
            }
        }
        Kind::Buddhist => (0, gy + 543, gm, gd),
        Kind::RepublicOfChina => {
            if gy >= 1912 {
                (1, gy - 1911, gm, gd)
            } else {
                (0, 1912 - gy, gm, gd)
            }
        }
        Kind::Japanese => {
            let era = JAPANESE_ERAS
                .iter()
                .rev()
                .find(|(y, m, d, _, _)| days >= days_from_civil(*y, *m, *d))
                .unwrap_or(&JAPANESE_ERAS[0]);
            (era.3, gy - era.0 + 1, gm, gd)
        }
        Kind::IslamicCivil => {
            let (y, m, d) = islamic_from_days(days);
            (0, y, m, d)
        }
    };
    CalDate { era, year, month, day, hour, minute, second, nanosecond, days, gregorian_year: gy }
}

/// Wall-clock seconds for a calendar date; months out of range roll into
/// other years and days out of range into other months.
#[allow(clippy::too_many_arguments)]
pub fn cal_wall(kind: Kind, era: i64, year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64, nanosecond: i64) -> f64 {
    let m0 = month - 1;
    let year = year + m0.div_euclid(12);
    let month = m0.rem_euclid(12) + 1;
    let first = match kind {
        Kind::IslamicCivil => islamic_to_jd(year, month, 1) - 2_440_588,
        _ => {
            let gy = match kind {
                Kind::Buddhist => year - 543,
                Kind::RepublicOfChina => {
                    if era == 0 {
                        1912 - year
                    } else {
                        year + 1911
                    }
                }
                Kind::Japanese => {
                    let start = JAPANESE_ERAS.iter().find(|e| e.3 == era).unwrap_or(&JAPANESE_ERAS[JAPANESE_ERAS.len() - 1]);
                    start.0 + year - 1
                }
                _ => {
                    if era == 0 {
                        1 - year
                    } else {
                        year
                    }
                }
            };
            days_from_civil(gy, month, 1)
        }
    };
    wall_of(first + day - 1, hour, minute, second, nanosecond)
}

/// The calendar used for adding and subtracting months: the Gregorian family
/// shares Gregorian months.
fn arithmetic_kind(kind: Kind) -> Kind {
    if kind == Kind::IslamicCivil {
        kind
    } else {
        Kind::Gregorian
    }
}

/// Adds months, keeping the time of day; the day is clamped to the month's end.
pub fn add_months(kind: Kind, wall: f64, months: i64) -> f64 {
    if months == 0 {
        return wall;
    }
    let kind = arithmetic_kind(kind);
    let c = cal_date(kind, wall);
    let total = c.year * 12 + c.month - 1 + months;
    let (year, month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let mut day = c.day;
    loop {
        let result = cal_wall(kind, c.era, year, month, day, c.hour, c.minute, c.second, c.nanosecond);
        if cal_date(kind, result).month == month || day <= 1 {
            return result;
        }
        day -= 1;
    }
}

/// First day of week 1 of a year (days since 1970).
fn week_one_start(year: i64, first_weekday: i64, min_days: i64) -> i64 {
    let jan1 = days_from_civil(year, 1, 1);
    let back = (weekday_of(jan1) - first_weekday).rem_euclid(7);
    let start = jan1 - back;
    if 7 - back >= min_days {
        start
    } else {
        start + 7
    }
}

/// (week of year, year it belongs to).
pub fn week_of_year(days: i64, first_weekday: i64, min_days: i64) -> (i64, i64) {
    let (year, _, _) = civil_from_days(days);
    for candidate in [year + 1, year, year - 1] {
        let start = week_one_start(candidate, first_weekday, min_days);
        if days >= start {
            return ((days - start) / 7 + 1, candidate);
        }
    }
    (1, year)
}

pub fn week_of_month(c: &CalDate, first_weekday: i64, min_days: i64) -> i64 {
    let first = c.days - c.day + 1;
    let back = (weekday_of(first) - first_weekday).rem_euclid(7);
    let week = (c.day - 1 + back) / 7 + 1;
    if 7 - back >= min_days {
        week
    } else {
        week - 1
    }
}

// ----- time zones -----

fn local_zone() -> Id {
    runtime::send(runtime::class_named("NSTimeZone"), "localTimeZone", &[])
}

fn local_parts(unix: f64) -> (i64, String) {
    unsafe {
        let t = unix.floor() as libc::time_t;
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut parts);
        let name = if parts.tm_zone.is_null() {
            "GMT".to_string()
        } else {
            std::ffi::CStr::from_ptr(parts.tm_zone).to_string_lossy().into_owned()
        };
        (parts.tm_gmtoff as i64, name)
    }
}

/// Seconds east of GMT for a zone (0: the phone's own) at a moment.
pub fn zone_offset(zone: Id, unix: f64) -> i64 {
    if zone == 0 || zone == local_zone() {
        return local_parts(unix).0;
    }
    let date = misc::date(unix - SINCE_1970);
    runtime::send(zone, "secondsFromGMTForDate:", &[date]) as isize as i64
}

fn zone_abbreviation(zone: Id, unix: f64) -> String {
    if zone == 0 || zone == local_zone() {
        let (offset, name) = local_parts(unix);
        // Bionic and glibc give "+03" style names where no letters exist.
        return if name.starts_with(['+', '-']) { gmt_long(offset) } else { name };
    }
    string::get(runtime::send(zone, "abbreviation", &[]))
}

/// The UTC moment of a wall-clock time in a zone.
pub fn wall_to_unix(zone: Id, wall: f64) -> f64 {
    let guess = wall - zone_offset(zone, wall) as f64;
    wall - zone_offset(zone, guess) as f64
}

pub fn unix_to_wall(zone: Id, unix: f64) -> f64 {
    unix + zone_offset(zone, unix) as f64
}

/// Seconds since 2001 of any NSDate.
pub fn date_seconds(date: Id) -> f64 {
    call_f64(date, "timeIntervalSinceReferenceDate")
}

pub fn call_f64(object: Id, name: &str) -> f64 {
    let s = runtime::sel(name);
    match runtime::imp_for(object, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
            f(object, s)
        }
        None => 0.0,
    }
}

fn log_once(what: String) {
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    if SEEN.lock().unwrap().get_or_insert_with(HashSet::new).insert(what.clone()) {
        crate::log!("{what}");
    }
}

// ----- NSCalendar: { isa, *mut Calendar } -----

struct Calendar {
    identifier: String,
    kind: Kind,
    zone: Id,
    locale: Id,
    first_weekday: i64,
    min_days: i64,
}

fn calendar(this: Id) -> &'static mut Calendar {
    unsafe {
        let slot = (this + 8) as *mut *mut Calendar;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(new_calendar("gregorian")));
        }
        &mut **slot
    }
}

fn new_calendar(identifier: &str) -> Calendar {
    let kind = kind_of(identifier).unwrap_or(Kind::Gregorian);
    if kind == Kind::Other {
        log_once(format!("NSCalendar {identifier}: computed with Gregorian years, months and days"));
    }
    let (first_weekday, min_days) = if kind == Kind::Iso8601 { (2, 4) } else { (1, 1) };
    Calendar { identifier: identifier.to_string(), kind, zone: 0, locale: 0, first_weekday, min_days }
}

fn make_calendar(class: Class, identifier: &str) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut *mut Calendar) = Box::into_raw(Box::new(new_calendar(identifier))) };
    object
}

extern "C" fn calendar_current(class: Class, _: Sel) -> Id {
    make_calendar(class, "gregorian")
}

extern "C" fn calendar_with_identifier(class: Class, _: Sel, identifier: Id) -> Id {
    let name = string::get(identifier);
    if kind_of(&name).is_none() {
        crate::log!("NSCalendar {name}: not a calendar iOS knows; nil");
        return 0;
    }
    make_calendar(class, &name)
}

extern "C" fn calendar_init_with_identifier(this: Id, _: Sel, identifier: Id) -> Id {
    let name = string::get(identifier);
    if kind_of(&name).is_none() {
        crate::log!("NSCalendar {name}: not a calendar iOS knows; nil");
        return 0;
    }
    unsafe { *((this + 8) as *mut *mut Calendar) = Box::into_raw(Box::new(new_calendar(&name))) };
    this
}

extern "C" fn calendar_init(this: Id, _: Sel) -> Id {
    calendar(this);
    this
}

extern "C" fn calendar_identifier(this: Id, _: Sel) -> Id {
    string::make(&calendar(this).identifier)
}

extern "C" fn calendar_time_zone(this: Id, _: Sel) -> Id {
    match calendar(this).zone {
        0 => local_zone(),
        zone => zone,
    }
}

extern "C" fn calendar_set_time_zone(this: Id, _: Sel, zone: Id) {
    calendar(this).zone = zone;
}

extern "C" fn calendar_locale(this: Id, _: Sel) -> Id {
    match calendar(this).locale {
        0 => runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]),
        locale => locale,
    }
}

extern "C" fn calendar_set_locale(this: Id, _: Sel, locale: Id) {
    calendar(this).locale = locale;
}

extern "C" fn calendar_first_weekday(this: Id, _: Sel) -> isize {
    calendar(this).first_weekday as isize
}

extern "C" fn calendar_set_first_weekday(this: Id, _: Sel, value: isize) {
    calendar(this).first_weekday = (value as i64).clamp(1, 7);
}

extern "C" fn calendar_min_days(this: Id, _: Sel) -> isize {
    calendar(this).min_days as isize
}

extern "C" fn calendar_set_min_days(this: Id, _: Sel, value: isize) {
    calendar(this).min_days = (value as i64).clamp(1, 7);
}

extern "C" fn calendar_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let c = calendar(this);
    let copy = make_calendar(runtime::class_named("NSCalendar"), &c.identifier);
    let d = calendar(copy);
    d.zone = c.zone;
    d.locale = c.locale;
    d.first_weekday = c.first_weekday;
    d.min_days = c.min_days;
    copy
}

// NSCalendarUnit bits.
const UNIT_ERA: u64 = 1 << 1;
const UNIT_YEAR: u64 = 1 << 2;
const UNIT_MONTH: u64 = 1 << 3;
const UNIT_DAY: u64 = 1 << 4;
const UNIT_HOUR: u64 = 1 << 5;
const UNIT_MINUTE: u64 = 1 << 6;
const UNIT_SECOND: u64 = 1 << 7;
const UNIT_WEEK: u64 = 1 << 8;
const UNIT_WEEKDAY: u64 = 1 << 9;
const UNIT_WEEKDAY_ORDINAL: u64 = 1 << 10;
const UNIT_QUARTER: u64 = 1 << 11;
const UNIT_WEEK_OF_MONTH: u64 = 1 << 12;
const UNIT_WEEK_OF_YEAR: u64 = 1 << 13;
const UNIT_YEAR_FOR_WEEK: u64 = 1 << 14;
const UNIT_NANOSECOND: u64 = 1 << 15;
const UNIT_CALENDAR: u64 = 1 << 20;
const UNIT_TIME_ZONE: u64 = 1 << 21;

// Component slots.
const ERA: usize = 0;
const YEAR: usize = 1;
const MONTH: usize = 2;
const DAY: usize = 3;
const HOUR: usize = 4;
const MINUTE: usize = 5;
const SECOND: usize = 6;
const NANOSECOND: usize = 7;
const WEEKDAY: usize = 8;
const WEEKDAY_ORDINAL: usize = 9;
const QUARTER: usize = 10;
const WEEK_OF_MONTH: usize = 11;
const WEEK_OF_YEAR: usize = 12;
const YEAR_FOR_WEEK: usize = 13;
const WEEK: usize = 14;
const SLOTS: usize = 15;

const UNIT_SLOTS: [(u64, usize); SLOTS] = [
    (UNIT_ERA, ERA),
    (UNIT_YEAR, YEAR),
    (UNIT_MONTH, MONTH),
    (UNIT_DAY, DAY),
    (UNIT_HOUR, HOUR),
    (UNIT_MINUTE, MINUTE),
    (UNIT_SECOND, SECOND),
    (UNIT_NANOSECOND, NANOSECOND),
    (UNIT_WEEKDAY, WEEKDAY),
    (UNIT_WEEKDAY_ORDINAL, WEEKDAY_ORDINAL),
    (UNIT_QUARTER, QUARTER),
    (UNIT_WEEK_OF_MONTH, WEEK_OF_MONTH),
    (UNIT_WEEK_OF_YEAR, WEEK_OF_YEAR),
    (UNIT_YEAR_FOR_WEEK, YEAR_FOR_WEEK),
    (UNIT_WEEK, WEEK),
];

/// Every field of a date in a calendar.
pub fn components_of(kind: Kind, wall: f64, first_weekday: i64, min_days: i64) -> [i64; SLOTS] {
    let c = cal_date(kind, wall);
    let (week, week_year) = week_of_year(c.days, first_weekday, min_days);
    let mut v = [0; SLOTS];
    v[ERA] = c.era;
    v[YEAR] = c.year;
    v[MONTH] = c.month;
    v[DAY] = c.day;
    v[HOUR] = c.hour;
    v[MINUTE] = c.minute;
    v[SECOND] = c.second;
    v[NANOSECOND] = c.nanosecond;
    v[WEEKDAY] = weekday_of(c.days);
    v[WEEKDAY_ORDINAL] = (c.day - 1) / 7 + 1;
    v[QUARTER] = (c.month - 1) / 3 + 1;
    v[WEEK_OF_MONTH] = week_of_month(&c, first_weekday, min_days);
    v[WEEK_OF_YEAR] = week;
    v[WEEK] = week;
    v[YEAR_FOR_WEEK] = week_year + (c.year - c.gregorian_year);
    v
}

/// The difference between two wall-clock times in the units asked for,
/// largest first (as NSCalendar does).
pub fn difference(kind: Kind, units: u64, from: f64, to: f64) -> [i64; SLOTS] {
    let mut v = [UNDEFINED; SLOTS];
    let mut current = from;
    let forward = to >= from;
    let step = if forward { 1 } else { -1 };
    let passed = |w: f64| if forward { w > to } else { w < to };
    let arithmetic = arithmetic_kind(kind);
    if units & UNIT_ERA != 0 {
        v[ERA] = 0;
    }
    if units & UNIT_YEAR != 0 {
        let (a, b) = (cal_date(arithmetic, current), cal_date(arithmetic, to));
        let mut n = b.year - a.year;
        while n != 0 && passed(add_months(kind, current, n * 12)) {
            n -= step;
        }
        v[YEAR] = n;
        current = add_months(kind, current, n * 12);
    }
    if units & UNIT_MONTH != 0 {
        let (a, b) = (cal_date(arithmetic, current), cal_date(arithmetic, to));
        let mut n = (b.year * 12 + b.month) - (a.year * 12 + a.month);
        while n != 0 && passed(add_months(kind, current, n)) {
            n -= step;
        }
        v[MONTH] = n;
        current = add_months(kind, current, n);
    }
    let mut rest = to - current;
    let mut take = |slot_units: &[(u64, usize)], size: f64, rest: &mut f64| {
        if slot_units.iter().any(|(u, _)| units & u != 0) {
            let n = (*rest / size).trunc();
            *rest -= n * size;
            for (u, slot) in slot_units {
                if units & u != 0 {
                    v[*slot] = n as i64;
                }
            }
        }
    };
    take(&[(UNIT_WEEK_OF_YEAR, WEEK_OF_YEAR), (UNIT_WEEK_OF_MONTH, WEEK_OF_MONTH), (UNIT_WEEK, WEEK)], 604_800.0, &mut rest);
    take(&[(UNIT_DAY, DAY)], 86_400.0, &mut rest);
    take(&[(UNIT_HOUR, HOUR)], 3600.0, &mut rest);
    take(&[(UNIT_MINUTE, MINUTE)], 60.0, &mut rest);
    take(&[(UNIT_SECOND, SECOND)], 1.0, &mut rest);
    if units & UNIT_NANOSECOND != 0 {
        v[NANOSECOND] = (rest * 1e9).round() as i64;
    }
    for (unit, slot) in [(UNIT_WEEKDAY, WEEKDAY), (UNIT_WEEKDAY_ORDINAL, WEEKDAY_ORDINAL), (UNIT_QUARTER, QUARTER), (UNIT_YEAR_FOR_WEEK, YEAR_FOR_WEEK)] {
        if units & unit != 0 {
            v[slot] = 0;
        }
    }
    v
}

/// The wall-clock time named by components (missing ones take iOS's defaults).
pub fn wall_from_components(kind: Kind, v: &[i64; SLOTS], first_weekday: i64, min_days: i64, now_era: i64) -> f64 {
    let get = |slot: usize, default: i64| if v[slot] == UNDEFINED { default } else { v[slot] };
    let (hour, minute, second, nanos) = (get(HOUR, 0), get(MINUTE, 0), get(SECOND, 0), get(NANOSECOND, 0));
    let week = if v[WEEK_OF_YEAR] != UNDEFINED { v[WEEK_OF_YEAR] } else { v[WEEK] };
    if v[MONTH] == UNDEFINED && v[DAY] == UNDEFINED && week != UNDEFINED && kind != Kind::IslamicCivil {
        let year = if v[YEAR_FOR_WEEK] != UNDEFINED { v[YEAR_FOR_WEEK] } else { get(YEAR, 1) };
        let gy = cal_date(kind, cal_wall(kind, get(ERA, now_era), year, 7, 1, 0, 0, 0, 0)).gregorian_year;
        let start = week_one_start(gy, first_weekday, min_days) + (week - 1) * 7;
        let weekday = get(WEEKDAY, first_weekday);
        let day = start + (weekday - first_weekday).rem_euclid(7);
        return wall_of(day, hour, minute, second, nanos);
    }
    let (era, year, month) = (get(ERA, now_era), get(YEAR, 1), get(MONTH, 1));
    if v[DAY] == UNDEFINED && v[WEEKDAY] != UNDEFINED {
        let first = cal_date(kind, cal_wall(kind, era, year, month, 1, 0, 0, 0, 0)).days;
        let ordinal = get(WEEKDAY_ORDINAL, 1);
        let day = first + (v[WEEKDAY] - weekday_of(first)).rem_euclid(7) + (ordinal - 1) * 7;
        return wall_of(day, hour, minute, second, nanos);
    }
    cal_wall(kind, era, year, month, get(DAY, 1), hour, minute, second, nanos)
}

// ----- NSDateComponents: { isa, *mut Components } -----

struct Components {
    values: [i64; SLOTS],
    calendar: Id,
    zone: Id,
    leap_month: bool,
}

fn components(this: Id) -> &'static mut Components {
    unsafe {
        let slot = (this + 8) as *mut *mut Components;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Components { values: [UNDEFINED; SLOTS], calendar: 0, zone: 0, leap_month: false }));
        }
        &mut **slot
    }
}

fn make_components(values: [i64; SLOTS]) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSDateComponents"));
    components(object).values = values;
    object
}

macro_rules! component {
    ($get:ident, $set:ident, $slot:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> isize {
            components(this).values[$slot] as isize
        }
        extern "C" fn $set(this: Id, _: Sel, value: isize) {
            components(this).values[$slot] = value as i64;
        }
    };
}

component!(comp_era, comp_set_era, ERA);
component!(comp_year, comp_set_year, YEAR);
component!(comp_month, comp_set_month, MONTH);
component!(comp_day, comp_set_day, DAY);
component!(comp_hour, comp_set_hour, HOUR);
component!(comp_minute, comp_set_minute, MINUTE);
component!(comp_second, comp_set_second, SECOND);
component!(comp_nanosecond, comp_set_nanosecond, NANOSECOND);
component!(comp_weekday, comp_set_weekday, WEEKDAY);
component!(comp_weekday_ordinal, comp_set_weekday_ordinal, WEEKDAY_ORDINAL);
component!(comp_quarter, comp_set_quarter, QUARTER);
component!(comp_week_of_month, comp_set_week_of_month, WEEK_OF_MONTH);
component!(comp_week_of_year, comp_set_week_of_year, WEEK_OF_YEAR);
component!(comp_year_for_week, comp_set_year_for_week, YEAR_FOR_WEEK);
component!(comp_week, comp_set_week, WEEK);

extern "C" fn comp_init(this: Id, _: Sel) -> Id {
    components(this);
    this
}

extern "C" fn comp_calendar(this: Id, _: Sel) -> Id {
    components(this).calendar
}

extern "C" fn comp_set_calendar(this: Id, _: Sel, value: Id) {
    components(this).calendar = value;
}

extern "C" fn comp_time_zone(this: Id, _: Sel) -> Id {
    components(this).zone
}

extern "C" fn comp_set_time_zone(this: Id, _: Sel, value: Id) {
    components(this).zone = value;
}

extern "C" fn comp_leap_month(this: Id, _: Sel) -> bool {
    components(this).leap_month
}

extern "C" fn comp_set_leap_month(this: Id, _: Sel, value: u8) {
    components(this).leap_month = value != 0;
}

extern "C" fn comp_date(this: Id, _: Sel) -> Id {
    let c = components(this);
    let calendar_object = if c.calendar != 0 { c.calendar } else { calendar_current(runtime::class_named("NSCalendar"), 0) };
    calendar_date_from_components(calendar_object, 0, this)
}

extern "C" fn comp_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let c = components(this);
    let copy = make_components(c.values);
    let d = components(copy);
    d.calendar = c.calendar;
    d.zone = c.zone;
    d.leap_month = c.leap_month;
    copy
}

// ----- NSCalendar methods that use components -----

fn wall_in(this: Id, date: Id) -> f64 {
    unix_to_wall(calendar(this).zone, date_seconds(date) + SINCE_1970)
}

fn date_from_wall(this: Id, wall: f64) -> Id {
    misc::date(wall_to_unix(calendar(this).zone, wall) - SINCE_1970)
}

extern "C" fn calendar_components_from_date(this: Id, _: Sel, units: u64, date: Id) -> Id {
    if date == 0 {
        return 0;
    }
    let c = calendar(this);
    let all = components_of(c.kind, wall_in(this, date), c.first_weekday, c.min_days);
    let mut values = [UNDEFINED; SLOTS];
    for (unit, slot) in UNIT_SLOTS {
        if units & unit != 0 {
            values[slot] = all[slot];
        }
    }
    let result = make_components(values);
    if units & UNIT_CALENDAR != 0 {
        components(result).calendar = this;
    }
    if units & UNIT_TIME_ZONE != 0 {
        components(result).zone = calendar_time_zone(this, 0);
    }
    result
}

extern "C" fn calendar_component_from_date(this: Id, s: Sel, unit: u64, date: Id) -> isize {
    let result = calendar_components_from_date(this, s, unit, date);
    UNIT_SLOTS.iter().find(|(u, _)| *u == unit).map(|(_, slot)| components(result).values[*slot] as isize).unwrap_or(0)
}

extern "C" fn calendar_difference(this: Id, _: Sel, units: u64, from: Id, to: Id, _options: u64) -> Id {
    if from == 0 || to == 0 {
        return 0;
    }
    let values = difference(calendar(this).kind, units, wall_in(this, from), wall_in(this, to));
    make_components(values)
}

extern "C" fn calendar_date_from_components(this: Id, _: Sel, comps: Id) -> Id {
    if comps == 0 {
        return 0;
    }
    let c = calendar(this);
    let given = components(comps);
    // A missing era is the one dates fall in today.
    let default_era = cal_date(c.kind, unix_to_wall(c.zone, misc::now_since_2001() + SINCE_1970)).era;
    let wall = wall_from_components(c.kind, &given.values, c.first_weekday, c.min_days, default_era);
    let zone = if given.zone != 0 { given.zone } else { c.zone };
    misc::date(wall_to_unix(zone, wall) - SINCE_1970)
}

/// Adds components largest first: years and months by the calendar, the rest
/// as wall-clock time.
fn add_components(kind: Kind, wall: f64, v: &[i64; SLOTS]) -> f64 {
    let get = |slot: usize| if v[slot] == UNDEFINED { 0 } else { v[slot] };
    let months = get(YEAR) * 12 + get(QUARTER) * 3 + get(MONTH);
    let weeks = get(WEEK_OF_YEAR) + get(WEEK_OF_MONTH) + get(WEEK);
    let days = get(DAY) + weeks * 7 + get(WEEKDAY) + get(WEEKDAY_ORDINAL) * 7;
    add_months(kind, wall, months)
        + (days * 86_400 + get(HOUR) * 3600 + get(MINUTE) * 60 + get(SECOND)) as f64
        + get(NANOSECOND) as f64 / 1e9
}

extern "C" fn calendar_date_by_adding(this: Id, _: Sel, comps: Id, date: Id, _options: u64) -> Id {
    if comps == 0 || date == 0 {
        return 0;
    }
    let wall = add_components(calendar(this).kind, wall_in(this, date), &components(comps).values);
    date_from_wall(this, wall)
}

extern "C" fn calendar_date_by_adding_unit(this: Id, _: Sel, unit: u64, value: isize, date: Id, _options: u64) -> Id {
    let mut values = [UNDEFINED; SLOTS];
    match UNIT_SLOTS.iter().find(|(u, _)| *u == unit) {
        Some((_, slot)) => values[*slot] = value as i64,
        None => return 0,
    }
    let wall = add_components(calendar(this).kind, wall_in(this, date), &values);
    date_from_wall(this, wall)
}

extern "C" fn calendar_start_of_day(this: Id, _: Sel, date: Id) -> Id {
    let wall = wall_in(this, date);
    date_from_wall(this, wall_of(split_wall(wall).0, 0, 0, 0, 0))
}

fn same_day(this: Id, a: Id, b: Id) -> bool {
    split_wall(wall_in(this, a)).0 == split_wall(wall_in(this, b)).0
}

extern "C" fn calendar_is_same_day(this: Id, _: Sel, a: Id, b: Id) -> bool {
    a != 0 && b != 0 && same_day(this, a, b)
}

extern "C" fn calendar_is_today(this: Id, _: Sel, date: Id) -> bool {
    date != 0 && same_day(this, date, misc::date(misc::now_since_2001()))
}

/// rangeOfUnit:inUnit:forDate: for the common pairs (days in a month or
/// year, months in a year, hours in a day...).
extern "C" fn calendar_range_of_unit(this: Id, _: Sel, smaller: u64, larger: u64, date: Id) -> string::Range {
    let c = calendar(this);
    let wall = wall_in(this, date);
    let now = cal_date(c.kind, wall);
    let range = |location: i64, length: i64| string::Range { location: location as usize, length: length as usize };
    let month_days = || {
        let start = cal_wall(c.kind, now.era, now.year, now.month, 1, 0, 0, 0, 0);
        let next = add_months(c.kind, start, 1);
        ((next - start) / 86_400.0).round() as i64
    };
    match (smaller, larger) {
        (UNIT_DAY, UNIT_MONTH) => range(1, month_days()),
        (UNIT_DAY, UNIT_YEAR) => {
            let start = cal_wall(c.kind, now.era, now.year, 1, 1, 0, 0, 0, 0);
            range(1, ((add_months(c.kind, start, 12) - start) / 86_400.0).round() as i64)
        }
        (UNIT_MONTH, UNIT_YEAR) => range(1, 12),
        (UNIT_HOUR, UNIT_DAY) => range(0, 24),
        (UNIT_MINUTE, UNIT_HOUR) | (UNIT_SECOND, UNIT_MINUTE) => range(0, 60),
        (UNIT_WEEKDAY, _) => range(1, 7),
        _ => {
            log_once(format!("NSCalendar rangeOfUnit {smaller:#x} inUnit {larger:#x}: not known"));
            range(i64::MAX, 0)
        }
    }
}

// ----- date formatting -----

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const ISLAMIC_MONTHS: [&str; 12] = [
    "Muharram", "Safar", "Rabiʻ I", "Rabiʻ II", "Jumada I", "Jumada II", "Rajab", "Shaʻban", "Ramadan", "Shawwal",
    "Dhuʻl-Qiʻdah", "Dhuʻl-Hijjah",
];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn month_name(kind: Kind, month: i64, width: usize) -> String {
    let index = (month - 1).clamp(0, 11) as usize;
    let full = if kind == Kind::IslamicCivil { ISLAMIC_MONTHS[index] } else { MONTHS[index] };
    match width {
        3 if kind == Kind::IslamicCivil => full.to_string(),
        3 => full.chars().take(3).collect(),
        5 => full.chars().take(1).collect(),
        _ => full.to_string(),
    }
}

fn weekday_name(weekday: i64, width: usize) -> String {
    let full = WEEKDAYS[(weekday - 1).clamp(0, 6) as usize];
    match width {
        4 => full.to_string(),
        5 => full.chars().take(1).collect(),
        6 => full.chars().take(2).collect(),
        _ => full.chars().take(3).collect(),
    }
}

fn era_name(kind: Kind, era: i64, width: usize) -> String {
    let (short, long) = match kind {
        Kind::Buddhist => ("BE", "Buddhist Era"),
        Kind::IslamicCivil => ("AH", "Anno Hegirae"),
        Kind::RepublicOfChina => {
            if era == 0 {
                ("Before R.O.C.", "Before R.O.C.")
            } else {
                ("Minguo", "Minguo")
            }
        }
        Kind::Japanese => {
            let name = JAPANESE_ERAS.iter().find(|e| e.3 == era).map(|e| e.4).unwrap_or("Meiji");
            (name, name)
        }
        _ => {
            if era == 0 {
                ("BC", "Before Christ")
            } else {
                ("AD", "Anno Domini")
            }
        }
    };
    match width {
        4 => long.to_string(),
        5 => short.chars().take(1).collect(),
        _ => short.to_string(),
    }
}

fn pad(value: i64, width: usize) -> String {
    if value < 0 {
        format!("-{:0width$}", -value)
    } else {
        format!("{value:0width$}")
    }
}

/// "GMT", or "GMT-08:00".
fn gmt_long(offset: i64) -> String {
    if offset == 0 {
        "GMT".into()
    } else {
        format!("GMT{}", iso_offset(offset, true, true))
    }
}

/// "+0530" / "+05:30" (seconds left out).
fn iso_offset(offset: i64, colon: bool, always_minutes: bool) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.abs() / 3600, offset.abs() / 60 % 60);
    if !always_minutes && minutes == 0 {
        format!("{sign}{hours:02}")
    } else if colon {
        format!("{sign}{hours:02}:{minutes:02}")
    } else {
        format!("{sign}{hours:02}{minutes:02}")
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Field(char, usize),
    Text(String),
}

fn tokens(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let push_text = |out: &mut Vec<Token>, text: &str| {
        if let Some(Token::Text(last)) = out.last_mut() {
            last.push_str(text);
        } else if !text.is_empty() {
            out.push(Token::Text(text.to_string()));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                push_text(&mut out, "'");
                i += 2;
                continue;
            }
            i += 1;
            let mut text = String::new();
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        text.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                text.push(chars[i]);
                i += 1;
            }
            push_text(&mut out, &text);
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i] == c {
                i += 1;
            }
            out.push(Token::Field(c, i - start));
        } else {
            push_text(&mut out, &c.to_string());
            i += 1;
        }
    }
    out
}

/// Everything a pattern needs to show one moment.
pub struct Moment {
    pub kind: Kind,
    /// Wall-clock seconds since 1970 in the formatter's zone.
    pub wall: f64,
    /// Seconds east of GMT.
    pub offset: i64,
    pub zone_short: String,
    pub first_weekday: i64,
    pub min_days: i64,
    pub am: String,
    pub pm: String,
}

pub fn format_date(pattern: &str, m: &Moment) -> String {
    let c = cal_date(m.kind, m.wall);
    let weekday = weekday_of(c.days);
    let mut out = String::new();
    for token in tokens(pattern) {
        let (letter, n) = match token {
            Token::Text(text) => {
                out.push_str(&text);
                continue;
            }
            Token::Field(letter, n) => (letter, n),
        };
        let text = match letter {
            'G' => era_name(m.kind, c.era, n),
            'y' if n == 2 => pad(c.year.rem_euclid(100), 2),
            'y' => pad(c.year, n),
            'Y' => {
                let year = week_of_year(c.days, m.first_weekday, m.min_days).1 + (c.year - c.gregorian_year);
                if n == 2 {
                    pad(year.rem_euclid(100), 2)
                } else {
                    pad(year, n)
                }
            }
            'u' => pad(c.gregorian_year, n),
            'Q' | 'q' => {
                let quarter = (c.month - 1) / 3 + 1;
                match n {
                    1 | 2 => pad(quarter, n),
                    3 => format!("Q{quarter}"),
                    _ => format!("{} quarter", ["1st", "2nd", "3rd", "4th"][(quarter - 1) as usize]),
                }
            }
            'M' | 'L' => match n {
                1 | 2 => pad(c.month, n),
                _ => month_name(m.kind, c.month, n),
            },
            'd' => pad(c.day, n),
            'D' => pad(c.days - days_from_civil(c.gregorian_year, 1, 1) + 1, n),
            'F' => pad((c.day - 1) / 7 + 1, n),
            'g' => pad(c.days + 2_440_588, n),
            'w' => pad(week_of_year(c.days, m.first_weekday, m.min_days).0, n),
            'W' => pad(week_of_month(&c, m.first_weekday, m.min_days), n),
            'E' => weekday_name(weekday, n),
            'e' | 'c' if n <= 2 => pad((weekday - m.first_weekday).rem_euclid(7) + 1, n),
            'e' | 'c' => weekday_name(weekday, n),
            'a' => (if c.hour < 12 { &m.am } else { &m.pm }).clone(),
            'h' => pad(if c.hour % 12 == 0 { 12 } else { c.hour % 12 }, n),
            'H' => pad(c.hour, n),
            'K' => pad(c.hour % 12, n),
            'k' => pad(if c.hour == 0 { 24 } else { c.hour }, n),
            'm' => pad(c.minute, n),
            's' => pad(c.second, n),
            'S' => {
                let digits = format!("{:09}", c.nanosecond);
                let mut text: String = digits.chars().take(n).collect();
                while text.len() < n {
                    text.push('0');
                }
                text
            }
            'A' => pad(((c.hour * 60 + c.minute) * 60 + c.second) * 1000 + c.nanosecond / 1_000_000, n),
            'z' if n <= 3 => m.zone_short.clone(),
            'z' => gmt_long(m.offset),
            'Z' => match n {
                1..=3 => iso_offset(m.offset, false, true),
                4 => gmt_long(m.offset),
                _ => {
                    if m.offset == 0 {
                        "Z".into()
                    } else {
                        iso_offset(m.offset, true, true)
                    }
                }
            },
            'O' if n == 1 => {
                if m.offset == 0 {
                    "GMT".into()
                } else {
                    let (hours, minutes) = (m.offset.abs() / 3600, m.offset.abs() / 60 % 60);
                    let sign = if m.offset < 0 { '-' } else { '+' };
                    if minutes == 0 {
                        format!("GMT{sign}{hours}")
                    } else {
                        format!("GMT{sign}{hours}:{minutes:02}")
                    }
                }
            }
            'O' => gmt_long(m.offset),
            'X' | 'x' => {
                if letter == 'X' && m.offset == 0 {
                    "Z".into()
                } else {
                    match n {
                        1 => iso_offset(m.offset, false, false),
                        2 | 4 => iso_offset(m.offset, false, true),
                        _ => iso_offset(m.offset, true, true),
                    }
                }
            }
            'v' | 'V' => m.zone_short.clone(),
            _ => {
                log_once(format!("NSDateFormatter: pattern letter {letter} not known; shown as is"));
                letter.to_string().repeat(n)
            }
        };
        out.push_str(&text);
    }
    out
}

/// What dateFromString: read from the text.
#[derive(Default, Debug, PartialEq)]
pub struct Parsed {
    pub era: Option<i64>,
    pub year: Option<i64>,
    pub month: Option<i64>,
    pub day: Option<i64>,
    pub hour: Option<i64>,
    pub minute: Option<i64>,
    pub second: Option<i64>,
    pub nanosecond: Option<i64>,
    /// Some(true) for PM.
    pub pm: Option<bool>,
    pub twelve_hour: bool,
    pub offset: Option<i64>,
}

const ZONE_ABBREVIATIONS: [(&str, i64); 14] = [
    ("UTC", 0),
    ("GMT", 0),
    ("UT", 0),
    ("Z", 0),
    ("EST", -5),
    ("EDT", -4),
    ("CST", -6),
    ("CDT", -5),
    ("MST", -7),
    ("MDT", -6),
    ("PST", -8),
    ("PDT", -7),
    ("BST", 1),
    ("CET", 1),
];

fn is_numeric_field(token: &Token) -> bool {
    match token {
        Token::Field(c, n) => match c {
            'y' | 'Y' | 'u' | 'd' | 'D' | 'F' | 'H' | 'h' | 'K' | 'k' | 'm' | 's' | 'S' | 'w' | 'W' | 'A' | 'g' => true,
            'M' | 'L' | 'Q' | 'q' | 'e' | 'c' => *n <= 2,
            _ => false,
        },
        Token::Text(_) => false,
    }
}

/// Reads one of `names` (longest match first, any case).
fn read_name(input: &[char], at: &mut usize, names: &[String]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (index, name) in names.iter().enumerate() {
        let name: Vec<char> = name.chars().collect();
        if name.is_empty() || *at + name.len() > input.len() {
            continue;
        }
        let matches = input[*at..*at + name.len()]
            .iter()
            .zip(&name)
            .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()));
        if matches && best.map(|(_, len)| name.len() > len).unwrap_or(true) {
            best = Some((index, name.len()));
        }
    }
    let (index, len) = best?;
    *at += len;
    Some(index)
}

fn read_digits(input: &[char], at: &mut usize, max: usize) -> Option<(i64, usize)> {
    let start = *at;
    while *at < input.len() && *at - start < max && input[*at].is_ascii_digit() {
        *at += 1;
    }
    if *at == start {
        return None;
    }
    let text: String = input[start..*at].iter().collect();
    Some((text.parse().ok()?, *at - start))
}

/// "+05:30", "+0530", "+05", "Z", "GMT", "GMT+5", "UTC-08:00", "PST".
fn read_zone(input: &[char], at: &mut usize) -> Option<i64> {
    let rest: String = input[*at..].iter().collect();
    for prefix in ["GMT", "UTC", "UT"] {
        if rest.starts_with(prefix) {
            *at += prefix.len();
            if *at < input.len() && (input[*at] == '+' || input[*at] == '-') {
                return read_signed_offset(input, at);
            }
            return Some(0);
        }
    }
    if *at < input.len() && (input[*at] == '+' || input[*at] == '-') {
        return read_signed_offset(input, at);
    }
    let names: Vec<String> = ZONE_ABBREVIATIONS.iter().map(|(n, _)| n.to_string()).collect();
    let index = read_name(input, at, &names)?;
    Some(ZONE_ABBREVIATIONS[index].1 * 3600)
}

fn read_signed_offset(input: &[char], at: &mut usize) -> Option<i64> {
    let sign = if input[*at] == '-' { -1 } else { 1 };
    *at += 1;
    let (first, digits) = read_digits(input, at, 4)?;
    let (hours, mut minutes) = match digits {
        1 | 2 => (first, 0),
        3 => (first / 100, first % 100),
        _ => (first / 100, first % 100),
    };
    if digits <= 2 && *at < input.len() && input[*at] == ':' {
        *at += 1;
        minutes = read_digits(input, at, 2)?.0;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Reads `text` by `pattern`; None when the text doesn't match all the way.
pub fn parse_date(pattern: &str, text: &str, kind: Kind, am: &str, pm: &str, this_year: i64) -> Option<Parsed> {
    let input: Vec<char> = text.chars().collect();
    let list = tokens(pattern);
    let mut at = 0;
    let mut p = Parsed::default();
    for (index, token) in list.iter().enumerate() {
        match token {
            Token::Text(literal) => {
                for wanted in literal.chars() {
                    if wanted.is_whitespace() {
                        if at >= input.len() || !input[at].is_whitespace() {
                            return None;
                        }
                        while at < input.len() && input[at].is_whitespace() {
                            at += 1;
                        }
                    } else {
                        let got = *input.get(at)?;
                        if !got.to_lowercase().eq(wanted.to_lowercase()) {
                            return None;
                        }
                        at += 1;
                    }
                }
            }
            Token::Field(letter, n) => {
                let n = *n;
                let adjacent = list.get(index + 1).map(is_numeric_field).unwrap_or(false);
                if is_numeric_field(token) {
                    let width = if !adjacent {
                        usize::MAX
                    } else if matches!(letter, 'y' | 'Y' | 'u') && n != 2 {
                        4
                    } else {
                        n
                    };
                    let negative = matches!(letter, 'y' | 'u') && input.get(at) == Some(&'-');
                    if negative {
                        at += 1;
                    }
                    let (value, digits) = read_digits(&input, &mut at, width)?;
                    let value = if negative { -value } else { value };
                    match letter {
                        'y' | 'Y' | 'u' => {
                            p.year = Some(if n == 2 && digits == 2 {
                                let century = this_year - this_year.rem_euclid(100);
                                let candidate = century + value;
                                if candidate > this_year + 20 {
                                    candidate - 100
                                } else {
                                    candidate
                                }
                            } else {
                                value
                            })
                        }
                        'M' | 'L' => p.month = Some(value),
                        'd' => p.day = Some(value),
                        'H' | 'k' => p.hour = Some(if *letter == 'k' && value == 24 { 0 } else { value }),
                        'h' | 'K' => {
                            p.hour = Some(value % 12);
                            p.twelve_hour = true;
                        }
                        'm' => p.minute = Some(value),
                        's' => p.second = Some(value),
                        'S' => {
                            let scale = 10i64.pow(9u32.saturating_sub(digits as u32));
                            p.nanosecond = Some(if digits > 9 { value / 10i64.pow(digits as u32 - 9) } else { value * scale });
                        }
                        _ => {}
                    }
                    continue;
                }
                match letter {
                    'M' | 'L' => {
                        let mut names: Vec<String> = (1..=12).map(|m| month_name(kind, m, 4)).collect();
                        names.extend((1..=12).map(|m| month_name(kind, m, 3)));
                        p.month = Some(read_name(&input, &mut at, &names)? as i64 % 12 + 1);
                    }
                    'E' | 'e' | 'c' => {
                        let mut names: Vec<String> = (1..=7).map(|d| weekday_name(d, 4)).collect();
                        names.extend((1..=7).map(|d| weekday_name(d, 3)));
                        read_name(&input, &mut at, &names)?;
                    }
                    'a' => {
                        let names = [am.to_string(), pm.to_string(), "a.m.".into(), "p.m.".into()];
                        p.pm = Some(read_name(&input, &mut at, &names)? % 2 == 1);
                    }
                    'G' => {
                        let eras: Vec<i64> = match kind {
                            Kind::Japanese => JAPANESE_ERAS.iter().map(|e| e.3).collect(),
                            _ => vec![0, 1],
                        };
                        let mut names: Vec<String> = eras.iter().map(|&e| era_name(kind, e, 4)).collect();
                        names.extend(eras.iter().map(|&e| era_name(kind, e, 3)));
                        p.era = Some(eras[read_name(&input, &mut at, &names)? % eras.len()]);
                    }
                    'Z' | 'z' | 'X' | 'x' | 'O' | 'v' | 'V' => p.offset = Some(read_zone(&input, &mut at)?),
                    'Q' | 'q' => {
                        // "Q1" or "1st quarter": read and ignore.
                        while at < input.len() && !input[at].is_whitespace() {
                            at += 1;
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    if at != input.len() {
        return None;
    }
    if p.twelve_hour && p.pm == Some(true) {
        p.hour = p.hour.map(|h| h + 12);
    }
    Some(p)
}

// ----- NSDateFormatter: { isa, *mut DateFormatter } -----

struct DateFormatter {
    format: Option<String>,
    date_style: i64,
    time_style: i64,
    zone: Id,
    locale: Id,
    calendar: Id,
    relative: bool,
    lenient: bool,
    behavior: i64,
    am: String,
    pm: String,
    default_date: Id,
}

fn formatter(this: Id) -> &'static mut DateFormatter {
    unsafe {
        let slot = (this + 8) as *mut *mut DateFormatter;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(DateFormatter {
                format: None,
                date_style: 0,
                time_style: 0,
                zone: 0,
                locale: 0,
                calendar: 0,
                relative: false,
                lenient: false,
                behavior: 1040,
                am: "AM".into(),
                pm: "PM".into(),
                default_date: 0,
            }));
        }
        &mut **slot
    }
}

const DATE_STYLES: [&str; 5] = ["", "M/d/yy", "MMM d, y", "MMMM d, y", "EEEE, MMMM d, y"];
const TIME_STYLES: [&str; 5] = ["", "h:mm a", "h:mm:ss a", "h:mm:ss a z", "h:mm:ss a zzzz"];

/// The pattern for date and time styles (English).
pub fn style_pattern(date_style: i64, time_style: i64) -> String {
    let date = DATE_STYLES.get(date_style as usize).copied().unwrap_or("");
    let time = TIME_STYLES.get(time_style as usize).copied().unwrap_or("");
    match (date.is_empty(), time.is_empty()) {
        (false, false) if date_style >= 3 => format!("{date} 'at' {time}"),
        (false, false) => format!("{date}, {time}"),
        (false, true) => date.to_string(),
        _ => time.to_string(),
    }
}

fn formatter_pattern(f: &DateFormatter) -> String {
    match &f.format {
        Some(format) => format.clone(),
        None => style_pattern(f.date_style, f.time_style),
    }
}

fn formatter_kind(f: &DateFormatter) -> (Kind, i64, i64) {
    if f.calendar != 0 {
        let c = calendar(f.calendar);
        return (c.kind, c.first_weekday, c.min_days);
    }
    (Kind::Gregorian, 1, 1)
}

fn check_locale(locale: Id) {
    if locale == 0 {
        return;
    }
    let identifier = string::get(runtime::send(locale, "localeIdentifier", &[]));
    if !identifier.is_empty() && !identifier.starts_with("en") {
        log_once(format!("NSDateFormatter locale {identifier}: English names and patterns used"));
    }
}

fn format_with(f: &DateFormatter, pattern: &str, date: Id) -> String {
    let unix = date_seconds(date) + SINCE_1970;
    let (kind, first_weekday, min_days) = formatter_kind(f);
    let moment = Moment {
        kind,
        wall: unix_to_wall(f.zone, unix),
        offset: zone_offset(f.zone, unix),
        zone_short: zone_abbreviation(f.zone, unix),
        first_weekday,
        min_days,
        am: f.am.clone(),
        pm: f.pm.clone(),
    };
    format_date(pattern, &moment)
}

/// "Today", "Yesterday", "Tomorrow" for a relative formatter, else None.
fn relative_day(f: &DateFormatter, date: Id) -> Option<&'static str> {
    let unix = date_seconds(date) + SINCE_1970;
    let now = misc::now_since_2001() + SINCE_1970;
    let day = split_wall(unix_to_wall(f.zone, unix)).0;
    let today = split_wall(unix_to_wall(f.zone, now)).0;
    match day - today {
        -1 => Some("Yesterday"),
        0 => Some("Today"),
        1 => Some("Tomorrow"),
        _ => None,
    }
}

extern "C" fn df_init(this: Id, _: Sel) -> Id {
    formatter(this);
    this
}

extern "C" fn df_string_from_date(this: Id, _: Sel, date: Id) -> Id {
    if date == 0 {
        return 0;
    }
    let f = formatter(this);
    check_locale(f.locale);
    if f.format.is_none() && f.relative && f.date_style != 0 {
        if let Some(word) = relative_day(f, date) {
            let time = style_pattern(0, f.time_style);
            if time.is_empty() {
                return string::make(word);
            }
            let joiner = if f.date_style >= 3 { " at " } else { ", " };
            return string::make(&format!("{word}{joiner}{}", format_with(f, &time, date)));
        }
    }
    string::make(&format_with(f, &formatter_pattern(f), date))
}

extern "C" fn df_date_from_string(this: Id, _: Sel, text: Id) -> Id {
    if text == 0 {
        return 0;
    }
    let f = formatter(this);
    let (kind, first_weekday, min_days) = formatter_kind(f);
    let now_wall = unix_to_wall(f.zone, misc::now_since_2001() + SINCE_1970);
    let this_year = cal_date(Kind::Gregorian, now_wall).year;
    let text = string::get(text);
    let source = if f.lenient { text.trim().to_string() } else { text.clone() };
    let Some(p) = parse_date(&formatter_pattern(f), &source, kind, &f.am, &f.pm, this_year) else {
        return 0;
    };
    // Missing fields come from the default date, else 2000-01-01 00:00.
    let base = if f.default_date != 0 {
        components_of(kind, unix_to_wall(f.zone, date_seconds(f.default_date) + SINCE_1970), first_weekday, min_days)
    } else {
        let mut v = components_of(kind, cal_wall(Kind::Gregorian, 1, 2000, 1, 1, 0, 0, 0, 0), first_weekday, min_days);
        v[HOUR] = 0;
        v
    };
    let wall = cal_wall(
        kind,
        p.era.unwrap_or(base[ERA]),
        p.year.unwrap_or(base[YEAR]),
        p.month.unwrap_or(base[MONTH]),
        p.day.unwrap_or(base[DAY]),
        p.hour.unwrap_or(if f.default_date != 0 { base[HOUR] } else { 0 }),
        p.minute.unwrap_or(if f.default_date != 0 { base[MINUTE] } else { 0 }),
        p.second.unwrap_or(if f.default_date != 0 { base[SECOND] } else { 0 }),
        p.nanosecond.unwrap_or(0),
    );
    let unix = match p.offset {
        Some(offset) => wall - offset as f64,
        None => wall_to_unix(f.zone, wall),
    };
    misc::date(unix - SINCE_1970)
}

extern "C" fn df_string_for_object(this: Id, s: Sel, object: Id) -> Id {
    let date_class = runtime::class_named("NSDate");
    let is_date = {
        let rt = runtime::lock();
        rt.is_subclass(rt.class_of(object), date_class)
    };
    if object == 0 || !is_date {
        return 0;
    }
    df_string_from_date(this, s, object)
}

extern "C" fn df_date_format(this: Id, _: Sel) -> Id {
    string::make(&formatter_pattern(formatter(this)))
}

extern "C" fn df_set_date_format(this: Id, _: Sel, format: Id) {
    formatter(this).format = if format == 0 { None } else { Some(string::get(format)) };
}

extern "C" fn df_date_style(this: Id, _: Sel) -> usize {
    formatter(this).date_style as usize
}

extern "C" fn df_set_date_style(this: Id, _: Sel, style: usize) {
    let f = formatter(this);
    f.date_style = (style as i64).clamp(0, 4);
    f.format = None;
}

extern "C" fn df_time_style(this: Id, _: Sel) -> usize {
    formatter(this).time_style as usize
}

extern "C" fn df_set_time_style(this: Id, _: Sel, style: usize) {
    let f = formatter(this);
    f.time_style = (style as i64).clamp(0, 4);
    f.format = None;
}

extern "C" fn df_locale(this: Id, _: Sel) -> Id {
    match formatter(this).locale {
        0 => runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]),
        locale => locale,
    }
}

extern "C" fn df_set_locale(this: Id, _: Sel, locale: Id) {
    formatter(this).locale = locale;
}

extern "C" fn df_time_zone(this: Id, _: Sel) -> Id {
    match formatter(this).zone {
        0 => local_zone(),
        zone => zone,
    }
}

extern "C" fn df_set_time_zone(this: Id, _: Sel, zone: Id) {
    formatter(this).zone = zone;
}

extern "C" fn df_calendar(this: Id, _: Sel) -> Id {
    let f = formatter(this);
    if f.calendar == 0 {
        f.calendar = calendar_current(runtime::class_named("NSCalendar"), 0);
    }
    f.calendar
}

extern "C" fn df_set_calendar(this: Id, _: Sel, value: Id) {
    formatter(this).calendar = value;
}

extern "C" fn df_relative(this: Id, _: Sel) -> bool {
    formatter(this).relative
}

extern "C" fn df_set_relative(this: Id, _: Sel, value: u8) {
    formatter(this).relative = value != 0;
}

extern "C" fn df_lenient(this: Id, _: Sel) -> bool {
    formatter(this).lenient
}

extern "C" fn df_set_lenient(this: Id, _: Sel, value: u8) {
    formatter(this).lenient = value != 0;
}

extern "C" fn df_behavior(this: Id, _: Sel) -> usize {
    formatter(this).behavior as usize
}

extern "C" fn df_set_behavior(this: Id, _: Sel, value: usize) {
    formatter(this).behavior = value as i64;
}

extern "C" fn df_am(this: Id, _: Sel) -> Id {
    string::make(&formatter(this).am)
}

extern "C" fn df_set_am(this: Id, _: Sel, value: Id) {
    formatter(this).am = string::get(value);
}

extern "C" fn df_pm(this: Id, _: Sel) -> Id {
    string::make(&formatter(this).pm)
}

extern "C" fn df_set_pm(this: Id, _: Sel, value: Id) {
    formatter(this).pm = string::get(value);
}

extern "C" fn df_default_date(this: Id, _: Sel) -> Id {
    formatter(this).default_date
}

extern "C" fn df_set_default_date(this: Id, _: Sel, value: Id) {
    formatter(this).default_date = value;
}

extern "C" fn df_month_symbols(_this: Id, _: Sel) -> Id {
    collections::array_from(&MONTHS.iter().map(|m| string::make(m)).collect::<Vec<_>>())
}

extern "C" fn df_short_month_symbols(_this: Id, _: Sel) -> Id {
    collections::array_from(&(1..=12).map(|m| string::make(&month_name(Kind::Gregorian, m, 3))).collect::<Vec<_>>())
}

extern "C" fn df_weekday_symbols(_this: Id, _: Sel) -> Id {
    collections::array_from(&WEEKDAYS.iter().map(|d| string::make(d)).collect::<Vec<_>>())
}

extern "C" fn df_short_weekday_symbols(_this: Id, _: Sel) -> Id {
    collections::array_from(&(1..=7).map(|d| string::make(&weekday_name(d, 3))).collect::<Vec<_>>())
}

extern "C" fn df_localized_string(class: Class, _: Sel, date: Id, date_style: usize, time_style: usize) -> Id {
    let object = runtime::alloc_instance(class);
    df_set_date_style(object, 0, date_style);
    df_set_time_style(object, 0, time_style);
    df_string_from_date(object, 0, date)
}

/// dateFormatFromTemplate:options:locale: puts the template's fields in
/// English order with English separators.
extern "C" fn df_format_from_template(_: Class, _: Sel, template: Id, _options: usize, _locale: Id) -> Id {
    let fields: Vec<(char, usize)> = tokens(&string::get(template))
        .into_iter()
        .filter_map(|t| match t {
            Token::Field(c, n) => Some((c, n)),
            Token::Text(_) => None,
        })
        .collect();
    let find = |letters: &str| fields.iter().find(|(c, _)| letters.contains(*c)).map(|&(c, n)| c.to_string().repeat(n));
    let (weekday, month, day, year) = (find("Ec"), find("ML"), find("d"), find("yY"));
    let textual_month = fields.iter().any(|(c, n)| "ML".contains(*c) && *n >= 3);
    let mut date = String::new();
    if textual_month {
        if let Some(weekday) = &weekday {
            date.push_str(&format!("{weekday}, "));
        }
        date.push_str(month.as_deref().unwrap_or(""));
        if let Some(day) = &day {
            date.push_str(&format!(" {day}"));
        }
        if let Some(year) = &year {
            date.push_str(&format!("{}{year}", if day.is_some() { ", " } else { " " }));
        }
    } else {
        let parts: Vec<String> = [month.clone(), day.clone(), year.clone()].into_iter().flatten().collect();
        if let Some(weekday) = &weekday {
            date.push_str(&format!("{weekday}, "));
        }
        date.push_str(&parts.join("/"));
    }
    let hour = find("hHkKj").map(|h| if h.starts_with('j') { "h".repeat(h.len()) } else { h });
    let mut time = String::new();
    if let Some(hour) = &hour {
        time.push_str(hour);
        if let Some(minute) = find("m") {
            time.push_str(&format!(":{minute}"));
        }
        if let Some(second) = find("s") {
            time.push_str(&format!(":{second}"));
        }
        if hour.starts_with(['h', 'K']) {
            time.push_str(" a");
        }
    } else if let Some(minute) = find("m") {
        time.push_str(&minute);
        if let Some(second) = find("s") {
            time.push_str(&format!(":{second}"));
        }
    }
    let joined = match (date.trim().is_empty(), time.is_empty()) {
        (false, false) => format!("{}, {time}", date.trim()),
        (false, true) => date.trim().to_string(),
        _ => time,
    };
    string::make(&joined)
}

extern "C" fn df_set_default_behavior(_: Class, _: Sel, _behavior: usize) {}

extern "C" fn df_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let f = formatter(this);
    let copy = runtime::alloc_instance(runtime::class_named("NSDateFormatter"));
    let g = formatter(copy);
    g.format = f.format.clone();
    g.date_style = f.date_style;
    g.time_style = f.time_style;
    g.zone = f.zone;
    g.locale = f.locale;
    g.calendar = f.calendar;
    g.relative = f.relative;
    g.lenient = f.lenient;
    g.behavior = f.behavior;
    g.am = f.am.clone();
    g.pm = f.pm.clone();
    g.default_date = f.default_date;
    copy
}

// ----- NSDate extras the calendar code needs -----

extern "C" fn date_init_interval_since(this: Id, _: Sel, seconds: f64, date: Id) -> Id {
    let base = if date == 0 { misc::now_since_2001() } else { date_seconds(date) };
    unsafe { *((this + 8) as *mut f64) = base + seconds };
    this
}

extern "C" fn date_with_interval_since(_: Class, _: Sel, seconds: f64, date: Id) -> Id {
    let base = if date == 0 { misc::now_since_2001() } else { date_seconds(date) };
    misc::date(base + seconds)
}

pub fn register(_: &mut Registry) {
    define_class("NSFormatter", "NSObject", 8);
    let calendar_class = define_class("NSCalendar", "NSObject", 16);
    class_methods(
        calendar_class,
        &[
            ("currentCalendar", calendar_current as usize),
            ("autoupdatingCurrentCalendar", calendar_current as usize),
            ("calendarWithIdentifier:", calendar_with_identifier as usize),
        ],
    );
    methods(
        calendar_class,
        &[
            ("init", calendar_init as usize),
            ("initWithCalendarIdentifier:", calendar_init_with_identifier as usize),
            ("calendarIdentifier", calendar_identifier as usize),
            ("timeZone", calendar_time_zone as usize),
            ("setTimeZone:", calendar_set_time_zone as usize),
            ("locale", calendar_locale as usize),
            ("setLocale:", calendar_set_locale as usize),
            ("firstWeekday", calendar_first_weekday as usize),
            ("setFirstWeekday:", calendar_set_first_weekday as usize),
            ("minimumDaysInFirstWeek", calendar_min_days as usize),
            ("setMinimumDaysInFirstWeek:", calendar_set_min_days as usize),
            ("copyWithZone:", calendar_copy as usize),
            ("components:fromDate:", calendar_components_from_date as usize),
            ("component:fromDate:", calendar_component_from_date as usize),
            ("components:fromDate:toDate:options:", calendar_difference as usize),
            ("dateFromComponents:", calendar_date_from_components as usize),
            ("dateByAddingComponents:toDate:options:", calendar_date_by_adding as usize),
            ("dateByAddingUnit:value:toDate:options:", calendar_date_by_adding_unit as usize),
            ("startOfDayForDate:", calendar_start_of_day as usize),
            ("isDate:inSameDayAsDate:", calendar_is_same_day as usize),
            ("isDateInToday:", calendar_is_today as usize),
            ("rangeOfUnit:inUnit:forDate:", calendar_range_of_unit as usize),
        ],
    );

    let comps = define_class("NSDateComponents", "NSObject", 16);
    methods(
        comps,
        &[
            ("init", comp_init as usize),
            ("era", comp_era as usize),
            ("setEra:", comp_set_era as usize),
            ("year", comp_year as usize),
            ("setYear:", comp_set_year as usize),
            ("month", comp_month as usize),
            ("setMonth:", comp_set_month as usize),
            ("day", comp_day as usize),
            ("setDay:", comp_set_day as usize),
            ("hour", comp_hour as usize),
            ("setHour:", comp_set_hour as usize),
            ("minute", comp_minute as usize),
            ("setMinute:", comp_set_minute as usize),
            ("second", comp_second as usize),
            ("setSecond:", comp_set_second as usize),
            ("nanosecond", comp_nanosecond as usize),
            ("setNanosecond:", comp_set_nanosecond as usize),
            ("weekday", comp_weekday as usize),
            ("setWeekday:", comp_set_weekday as usize),
            ("weekdayOrdinal", comp_weekday_ordinal as usize),
            ("setWeekdayOrdinal:", comp_set_weekday_ordinal as usize),
            ("quarter", comp_quarter as usize),
            ("setQuarter:", comp_set_quarter as usize),
            ("weekOfMonth", comp_week_of_month as usize),
            ("setWeekOfMonth:", comp_set_week_of_month as usize),
            ("weekOfYear", comp_week_of_year as usize),
            ("setWeekOfYear:", comp_set_week_of_year as usize),
            ("yearForWeekOfYear", comp_year_for_week as usize),
            ("setYearForWeekOfYear:", comp_set_year_for_week as usize),
            ("week", comp_week as usize),
            ("setWeek:", comp_set_week as usize),
            ("calendar", comp_calendar as usize),
            ("setCalendar:", comp_set_calendar as usize),
            ("timeZone", comp_time_zone as usize),
            ("setTimeZone:", comp_set_time_zone as usize),
            ("isLeapMonth", comp_leap_month as usize),
            ("setLeapMonth:", comp_set_leap_month as usize),
            ("date", comp_date as usize),
            ("copyWithZone:", comp_copy as usize),
        ],
    );

    let date_formatter = define_class("NSDateFormatter", "NSFormatter", 16);
    class_methods(
        date_formatter,
        &[
            ("localizedStringFromDate:dateStyle:timeStyle:", df_localized_string as usize),
            ("dateFormatFromTemplate:options:locale:", df_format_from_template as usize),
            ("setDefaultFormatterBehavior:", df_set_default_behavior as usize),
        ],
    );
    methods(
        date_formatter,
        &[
            ("init", df_init as usize),
            ("stringFromDate:", df_string_from_date as usize),
            ("dateFromString:", df_date_from_string as usize),
            ("stringForObjectValue:", df_string_for_object as usize),
            ("dateFormat", df_date_format as usize),
            ("setDateFormat:", df_set_date_format as usize),
            ("dateStyle", df_date_style as usize),
            ("setDateStyle:", df_set_date_style as usize),
            ("timeStyle", df_time_style as usize),
            ("setTimeStyle:", df_set_time_style as usize),
            ("locale", df_locale as usize),
            ("setLocale:", df_set_locale as usize),
            ("timeZone", df_time_zone as usize),
            ("setTimeZone:", df_set_time_zone as usize),
            ("calendar", df_calendar as usize),
            ("setCalendar:", df_set_calendar as usize),
            ("doesRelativeDateFormatting", df_relative as usize),
            ("setDoesRelativeDateFormatting:", df_set_relative as usize),
            ("isLenient", df_lenient as usize),
            ("setLenient:", df_set_lenient as usize),
            ("formatterBehavior", df_behavior as usize),
            ("setFormatterBehavior:", df_set_behavior as usize),
            ("AMSymbol", df_am as usize),
            ("setAMSymbol:", df_set_am as usize),
            ("PMSymbol", df_pm as usize),
            ("setPMSymbol:", df_set_pm as usize),
            ("defaultDate", df_default_date as usize),
            ("setDefaultDate:", df_set_default_date as usize),
            ("monthSymbols", df_month_symbols as usize),
            ("shortMonthSymbols", df_short_month_symbols as usize),
            ("weekdaySymbols", df_weekday_symbols as usize),
            ("shortWeekdaySymbols", df_short_weekday_symbols as usize),
            ("copyWithZone:", df_copy as usize),
        ],
    );

    let date = runtime::class_named("NSDate");
    if date != 0 {
        methods(date, &[("initWithTimeInterval:sinceDate:", date_init_interval_since as usize)]);
        class_methods(date, &[("dateWithTimeInterval:sinceDate:", date_with_interval_since as usize)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(wall: f64, offset: i64, zone: &str) -> Moment {
        Moment {
            kind: Kind::Gregorian,
            wall,
            offset,
            zone_short: zone.into(),
            first_weekday: 1,
            min_days: 1,
            am: "AM".into(),
            pm: "PM".into(),
        }
    }

    #[test]
    fn date_patterns_format_like_ios() {
        // 2026-10-21 14:05:09.250 wall clock, six hours behind GMT.
        let wall = cal_wall(Kind::Gregorian, 1, 2026, 10, 21, 14, 5, 9, 250_000_000);
        let m = moment(wall, -6 * 3600, "MDT");
        assert_eq!(format_date("yyyy-MM-dd'T'HH:mm:ss'Z'Z", &m), "2026-10-21T14:05:09Z-0600");
        assert_eq!(format_date(&style_pattern(2, 1), &m), "Oct 21, 2026, 2:05 PM");
        assert_eq!(format_date(&style_pattern(4, 3), &m), "Wednesday, October 21, 2026 at 2:05:09 PM MDT");
        assert_eq!(format_date("EEE d MMM yy h:mm:ss.SSS a ZZZZZ 'o''clock' w", &m), "Wed 21 Oct 26 2:05:09.250 PM -06:00 o'clock 43");
        // Era years of the calendars built on Gregorian.
        let japanese = Moment { kind: Kind::Japanese, ..moment(wall, 0, "GMT") };
        assert_eq!(format_date("G y", &japanese), "Reiwa 8");
        let buddhist = Moment { kind: Kind::Buddhist, ..moment(wall, 0, "GMT") };
        assert_eq!(format_date("y G", &buddhist), "2569 BE");
        let islamic = Moment { kind: Kind::IslamicCivil, ..moment(cal_wall(Kind::Gregorian, 1, 1970, 1, 1, 0, 0, 0, 0), 0, "GMT") };
        assert_eq!(format_date("d M y", &islamic), "22 10 1389");
    }

    #[test]
    fn date_strings_parse_back() {
        let p = parse_date("yyyy-MM-dd'T'HH:mm:ssZZZZZ", "2013-02-28T23:59:01+05:30", Kind::Gregorian, "AM", "PM", 2026).unwrap();
        assert_eq!((p.year, p.month, p.day, p.hour, p.minute, p.second), (Some(2013), Some(2), Some(28), Some(23), Some(59), Some(1)));
        assert_eq!(p.offset, Some(5 * 3600 + 30 * 60));
        let p = parse_date("MMM d, yy h:mm a", "jan 5, 98 12:30 am", Kind::Gregorian, "AM", "PM", 2026).unwrap();
        assert_eq!((p.year, p.month, p.day, p.hour), (Some(1998), Some(1), Some(5), Some(0)));
        let p = parse_date("yyyyMMddHHmm", "202610211405", Kind::Gregorian, "AM", "PM", 2026).unwrap();
        assert_eq!((p.year, p.month, p.day, p.hour, p.minute), (Some(2026), Some(10), Some(21), Some(14), Some(5)));
        assert_eq!(parse_date("yyyy-MM-dd", "2026-10-21 extra", Kind::Gregorian, "AM", "PM", 2026), None);
    }

    #[test]
    fn calendar_math_matches_ios() {
        let jan31 = cal_wall(Kind::Gregorian, 1, 2024, 1, 31, 10, 0, 0, 0);
        let feb = cal_date(Kind::Gregorian, add_months(Kind::Gregorian, jan31, 1));
        assert_eq!((feb.year, feb.month, feb.day, feb.hour), (2024, 2, 29, 10));
        let later = cal_wall(Kind::Gregorian, 1, 2025, 3, 1, 9, 30, 15, 0);
        let units = UNIT_YEAR | UNIT_MONTH | UNIT_DAY | UNIT_HOUR | UNIT_MINUTE | UNIT_SECOND;
        let d = difference(Kind::Gregorian, units, jan31, later);
        assert_eq!((d[YEAR], d[MONTH], d[DAY], d[HOUR], d[MINUTE], d[SECOND]), (1, 1, 0, 23, 30, 15));
        let back = difference(Kind::Gregorian, UNIT_DAY, later, jan31);
        assert_eq!(back[DAY], -394);
        // 2027-01-01 is a Friday: US weeks put it in week 1, ISO in 2026's week 53.
        let day = days_from_civil(2027, 1, 1);
        assert_eq!(weekday_of(day), 6);
        assert_eq!(week_of_year(day, 1, 1), (1, 2027));
        assert_eq!(week_of_year(day, 2, 4), (53, 2026));
        assert_eq!(week_of_year(days_from_civil(2026, 12, 28), 1, 1), (1, 2027));
    }
}
