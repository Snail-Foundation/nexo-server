use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};

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
