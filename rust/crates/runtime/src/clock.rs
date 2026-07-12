//! Календарное время без внешних зависимостей.
//!
//! Единый источник timestamp'ов для манифестов агентов, задач и системного
//! промпта. Раньше «ISO»-метки были epoch-секундами в строке, а дата в промпте
//! была захардкожена на дату релиза — модель не могла понять, сколько времени
//! прошло с запуска фоновой задачи. Всё в UTC.

// Целочисленная календарная арифметика: все касты ограничены диапазоном
// unix-времени (дни ≪ i64::MAX, компоненты даты ≪ u32::MAX) и не переполняются.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::time::{SystemTime, UNIX_EPOCH};

/// Текущее unix-время в секундах.
#[must_use]
pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Текущий момент как `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn iso8601_now() -> String {
    epoch_secs_to_iso8601(now_epoch_secs())
}

/// Сегодняшняя дата как `YYYY-MM-DD` (для системного промпта).
#[must_use]
pub fn current_date_utc() -> String {
    let (year, month, day) = civil_from_days((now_epoch_secs() / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Unix-секунды → `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn epoch_secs_to_iso8601(secs: u64) -> String {
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Обратный разбор меток, созданных [`epoch_secs_to_iso8601`]. Возвращает
/// `None` для чужих/старых форматов (в т.ч. голых epoch-секунд из прежних
/// версий манифестов).
#[must_use]
pub fn iso8601_to_epoch_secs(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| value[range].parse::<u64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year as i64, month as u32, day as u32);
    u64::try_from(days * 86_400 + (hour * 3600 + minute * 60 + second) as i64).ok()
}

/// Дни с 1970-01-01 → (год, месяц, день). Алгоритм Говарда Хиннанта
/// (<https://howardhinnant.github.io/date_algorithms.html>), точен для всего
/// диапазона unix-времени.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// (год, месяц, день) → дни с 1970-01-01 (обратная к [`civil_from_days`]).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u64;
    let month = u64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + u64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::{epoch_secs_to_iso8601, iso8601_to_epoch_secs};

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(epoch_secs_to_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_secs_to_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(epoch_secs_to_iso8601(1_752_105_600), "2025-07-10T00:00:00Z");
        assert_eq!(epoch_secs_to_iso8601(1_767_225_599), "2025-12-31T23:59:59Z");
    }

    #[test]
    fn round_trips_across_years_and_leap_days() {
        for secs in [0_u64, 86_399, 951_782_400, 1_752_105_600, 4_102_444_800] {
            let iso = epoch_secs_to_iso8601(secs);
            assert_eq!(iso8601_to_epoch_secs(&iso), Some(secs), "roundtrip {iso}");
        }
    }

    #[test]
    fn rejects_foreign_formats() {
        assert_eq!(iso8601_to_epoch_secs("1752105600"), None);
        assert_eq!(iso8601_to_epoch_secs("2025-07-10"), None);
        assert_eq!(iso8601_to_epoch_secs("2025-07-10 12:00:00"), None);
        assert_eq!(iso8601_to_epoch_secs("2025-13-10T00:00:00Z"), None);
        assert_eq!(iso8601_to_epoch_secs("2025-07-10T25:00:00Z"), None);
    }
}
