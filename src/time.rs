use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDate, TimeZone};

/// Current time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Formats a timestamp as local `HH:MM:SS`.
pub fn format_local_time(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|datetime| datetime.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_owned())
}

/// Formats a timestamp as local `YYYY-MM-DD HH:MM`.
pub fn format_local_datetime(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "---- --:--".to_owned())
}

/// Returns today's local date as `YYYY-MM-DD`.
pub fn today_local_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// Returns the local half-open day bounds `[start, end)` for a date.
pub fn local_day_bounds(date: &str) -> Option<(i64, i64)> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let start = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    let next = day.succ_opt()?;
    let end = Local
        .from_local_datetime(&next.and_hms_opt(0, 0, 0)?)
        .earliest()?;

    Some((start.timestamp_millis(), end.timestamp_millis()))
}
