// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded RFC6265 cookie date token parser.
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub(super) fn parse(text: &str) -> Option<SystemTime> {
    let mut time = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;
    for token in text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == ':'))
        .filter(|s| !s.is_empty())
    {
        if time.is_none() {
            if let Some((h, r)) = token.split_once(':') {
                if let Some((m, s)) = r.split_once(':') {
                    if let (Some(h), Some(m), Some(s)) = (
                        h.parse::<u32>().ok(),
                        m.parse::<u32>().ok(),
                        s.parse::<u32>().ok(),
                    ) {
                        if h < 24 && m < 60 && s < 60 {
                            time = Some((h, m, s));
                            continue;
                        }
                    }
                }
            }
        }
        if day.is_none() && token.len() <= 2 {
            if let Ok(n) = token.parse::<u32>() {
                if (1..=31).contains(&n) {
                    day = Some(n);
                    continue;
                }
            }
        }
        if month.is_none() && token.len() >= 3 {
            let first = token.get(..3)?.to_ascii_lowercase();
            if let Some(value) = match first.as_str() {
                "jan" => Some(1),
                "feb" => Some(2),
                "mar" => Some(3),
                "apr" => Some(4),
                "may" => Some(5),
                "jun" => Some(6),
                "jul" => Some(7),
                "aug" => Some(8),
                "sep" => Some(9),
                "oct" => Some(10),
                "nov" => Some(11),
                "dec" => Some(12),
                _ => None,
            } {
                month = Some(value);
                continue;
            }
        }
        if year.is_none() && token.len() <= 4 {
            if let Ok(n) = token.parse::<i32>() {
                year = Some(n)
            }
        }
    }
    let (h, mi, s) = time?;
    let mut y = year?;
    if (0..=69).contains(&y) {
        y += 2000
    } else if (70..=99).contains(&y) {
        y += 1900
    }
    if y < 1601 {
        return None;
    }
    let m = month?;
    let d = day?;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if d > max {
        return None;
    }
    let y0 = y - if m <= 2 { 1 } else { 0 };
    let era = y0.div_euclid(400);
    let yoe = y0 - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146097 + doe as i64 - 719468;
    let sec = days
        .checked_mul(86400)?
        .checked_add((h * 3600 + mi * 60 + s) as i64)?;
    if sec < 0 {
        UNIX_EPOCH.checked_sub(Duration::from_secs(sec.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_add(Duration::from_secs(sec as u64))
    }
}
