//! Date serial numbers (1900 date system, including the historical 1900-02-29 quirk).
//!
//! Serial 1 = 1900-01-01. Serial 60 = the non-existent 1900-02-29 kept for compatibility.
//! Serial 61 = 1900-03-01. Fractional part is time of day.

/// Days from civil date (proleptic Gregorian) to days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 1899-12-30 in days since epoch; serial = days - this (for dates after 1900-03-01).
const BASE: i64 = -25_569;

/// Serial number for a year/month/day, normalising overflowing months and days the way the
/// `DATE` function does. Returns `None` outside 1900-01-00 ..= 9999-12-31.
pub fn date_to_serial(year: i64, month: i64, day: i64) -> Option<f64> {
    let year = if (0..1900).contains(&year) {
        year + 1900
    } else {
        year
    };
    let total_months = year.checked_mul(12)?.checked_add(month - 1)?;
    let y = total_months.div_euclid(12);
    let m = (total_months.rem_euclid(12) + 1) as u32;
    let first = days_from_civil(y, m, 1);
    let days = first.checked_add(day - 1)?;
    let mut serial = days - BASE;
    // Before 1900-03-01 the target counts the phantom Feb 29.
    if serial < 61 {
        serial -= 1;
    }
    if !(0..=2_958_465).contains(&serial) {
        return None;
    }
    Some(serial as f64)
}

/// (year, month, day) of a serial. Serial 0 → (1900, 1, 0); serial 60 → (1900, 2, 29).
pub fn serial_to_date(serial: f64) -> Option<(i64, u32, u32)> {
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let s = serial.floor() as i64;
    match s {
        0 => Some((1900, 1, 0)),
        60 => Some((1900, 2, 29)),
        _ => {
            let days = if s < 60 { s + 1 } else { s } + BASE;
            Some(civil_from_days(days))
        }
    }
}

/// (hour, minute, second) of the fractional part, rounded to the nearest second.
pub fn serial_to_time(serial: f64) -> (u32, u32, u32) {
    let frac = serial - serial.floor();
    let secs = (frac * 86_400.0).round() as i64 % 86_400;
    (
        (secs / 3600) as u32,
        ((secs % 3600) / 60) as u32,
        (secs % 60) as u32,
    )
}

/// Day of week of a serial, 0 = Sunday (matching the target's 1900 quirk: serial 1 is Sunday).
pub fn weekday(serial: f64) -> u32 {
    // Serials count straight through the phantom 1900-02-29, so no correction is needed.
    let s = serial.floor() as i64;
    ((s - 1).rem_euclid(7)) as u32
}

pub fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
    }
}

/// Current local-agnostic time as a serial (UTC; the shell may adjust for timezone).
pub fn now_serial() -> f64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    secs / 86_400.0 - BASE as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_serials() {
        assert_eq!(date_to_serial(1900, 1, 1), Some(1.0));
        assert_eq!(date_to_serial(1900, 2, 28), Some(59.0));
        assert_eq!(date_to_serial(1900, 3, 1), Some(61.0));
        assert_eq!(date_to_serial(2024, 1, 15), Some(45306.0));
        assert_eq!(date_to_serial(2008, 14, 1), date_to_serial(2009, 2, 1));
        assert_eq!(date_to_serial(108, 1, 1), date_to_serial(2008, 1, 1));
        assert_eq!(serial_to_date(45306.0), Some((2024, 1, 15)));
        assert_eq!(serial_to_date(60.0), Some((1900, 2, 29)));
        assert_eq!(serial_to_date(1.0), Some((1900, 1, 1)));
        assert_eq!(serial_to_date(61.0), Some((1900, 3, 1)));
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday(1.0), 0); // 1900-01-01 is a Sunday in the target's calendar
        assert_eq!(weekday(45306.0), 1); // 2024-01-15 Monday
    }

    #[test]
    fn times() {
        assert_eq!(serial_to_time(0.5), (12, 0, 0));
        assert_eq!(serial_to_time(45306.75), (18, 0, 0));
    }
}
