use ncm_model::Timestamp;

pub fn now() -> Timestamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp(millis.min(i64::MAX as u128) as i64)
}

/// RFC3339 to epoch milliseconds, including fractions and numeric offsets, without a date crate.
pub fn parse_timestamp(text: &str) -> Option<Timestamp> {
    let (date, time) = text.split_once(['T', 't'])?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    let (clock, offset) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let idx = time.find(['+', '-'])?;
        let zone = &time[idx + 1..];
        let (hours, minutes) = zone.split_once(':')?;
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
            return None;
        }
        (
            &time[..idx],
            (hours * 60 + minutes) * if time.as_bytes()[idx] == b'+' { 1 } else { -1 },
        )
    };
    let mut clock_parts = clock.split(':');
    let hours: i64 = clock_parts.next()?.parse().ok()?;
    let minutes: i64 = clock_parts.next()?.parse().ok()?;
    let seconds = clock_parts.next()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let seconds: i64 = seconds.parse().ok()?;
    if clock_parts.next().is_some()
        || !(0..24).contains(&hours)
        || !(0..60).contains(&minutes)
        || !(0..=60).contains(&seconds)
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut millis = 0;
    for (i, byte) in fraction.bytes().take(3).enumerate() {
        millis += (byte - b'0') as i64 * [100, 10, 1][i];
    }
    // Gregorian civil-date algorithm, with March as month zero.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(Timestamp(
        ((days * 24 + hours) * 60 + minutes - offset) * 60 * 1000 + seconds * 1000 + millis,
    ))
}
