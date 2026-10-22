// ---------------------------------------------------------------------------
// Registration profile: `time_zone` / `date_of_birth` + min-age policy.
// ---------------------------------------------------------------------------

/// Minimum age at registration, in full years.
///
/// Upstream collects `date_of_birth` for age assurance; the operator-facing
/// override (`minimum_age_years` config key) is proposed in the G-E report â€”
/// until it lands this constant is the policy. Rationale: 16 is the GDPR
/// default age of digital consent and the strictest EU member-state rule, so
/// one static gate satisfies every member state.
pub const MINIMUM_AGE_YEARS: i64 = 16;

/// Validate a captured time-zone name.
///
/// Upstream validates inclusion in the IANA database
/// (`ActiveSupport::TimeZone`); without a tz database this validates the
/// `Area/Location` shape (plus bare `UTC`, which Rails also accepts):
/// 1â€“64 chars of letters, digits, `_`, `-`, `+` with at least one `/`
/// separating two non-empty parts. Anything else is a 422
/// (`Error::InvalidInput`, rendered `Validation failed: ...`).
pub fn validate_time_zone(raw: &str) -> crate::domain::Result<()> {
    use crate::domain::Error;
    let tz = raw.trim();
    if tz.eq_ignore_ascii_case("utc") {
        return Ok(());
    }
    let mut parts = tz.split('/');
    let area_ok = matches!(parts.next(), Some(area) if !area.is_empty());
    let rest: Vec<&str> = parts.collect();
    let shape_ok = !tz.is_empty()
        && tz.len() <= 64
        && area_ok
        && !rest.is_empty()
        && rest.iter().all(|p| !p.is_empty())
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'));
    if shape_ok {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "Validation failed: Time zone is invalid".into(),
        ))
    }
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Parse a strict `YYYY-MM-DD` calendar date into `(year, month, day)`.
fn parse_iso_date(raw: &str) -> Option<(i64, i64, i64)> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = raw[0..4].parse().ok()?;
    let month: i64 = raw[5..7].parse().ok()?;
    let day: i64 = raw[8..10].parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

/// Enforce the min-age policy on a captured date of birth.
///
/// `raw` must be a real past calendar date in `YYYY-MM-DD` form showing an
/// age of at least [`MINIMUM_AGE_YEARS`] full years at `now_ms` (UTC day
/// precision). Malformed, impossible (2021-02-29), future, or under-age
/// dates are `Error::InvalidInput`, which the API layer renders as the
/// upstream-shaped `422 {"error":"Validation failed: ..."}`.
pub fn check_date_of_birth(raw: &str, now_ms: u64) -> crate::domain::Result<()> {
    check_date_of_birth_with_min_age(raw, now_ms, MINIMUM_AGE_YEARS as u8)
}

/// [`check_date_of_birth`] with an operator-configured minimum age
/// (`Config::minimum_age_years`, 13â€“21). Same shapes, parameterized message.
pub fn check_date_of_birth_with_min_age(
    raw: &str,
    now_ms: u64,
    minimum_age_years: u8,
) -> crate::domain::Result<()> {
    use crate::domain::Error;
    let raw = raw.trim();
    let Some((year, month, day)) = parse_iso_date(raw) else {
        return Err(Error::InvalidInput(
            "Validation failed: Date of birth is invalid".into(),
        ));
    };
    let today_days = (now_ms / 86_400_000) as i64;
    if days_from_civil(year, month, day) > today_days {
        return Err(Error::InvalidInput(
            "Validation failed: Date of birth is invalid".into(),
        ));
    }
    // Full-years age from the civil dates (re-derived from day counts is
    // leap-unsafe, so compare month/day directly against the epoch civil).
    let civil = civil_from_days(today_days);
    let mut age = civil.0 - year;
    if (civil.1, civil.2) < (month, day) {
        age -= 1;
    }
    if age < minimum_age_years as i64 {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Date of birth must indicate an age of at least {minimum_age_years} years"
        )));
    }
    Ok(())
}

/// Civil `(year, month, day)` for days since the Unix epoch (inverse of
/// [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}
