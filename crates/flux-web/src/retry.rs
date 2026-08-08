//! Rate-limit recovery for the guarded HTTP family (C-701) — when to try a 429 again, and for how
//! long to wait first.
//!
//! ## Why a 429 and nothing else
//!
//! A `429 Too Many Requests` is a **definite answer**: the far side received the request, declined
//! to act on it, and said when to come back. That is what makes retrying sound for *any* method,
//! including a POST — the server is telling you it did not act on it, so a second attempt cannot
//! duplicate an effect. It is categorically different from a transport failure, where the request
//! may or may not have been processed, and it is why C-674's framed HTTP route carrying no
//! at-most-once guarantee does not block this: nothing here retries an *unanswered* request.
//!
//! **503 is deliberately excluded**, and the reason is the same sentence read backwards. A
//! `503 Service Unavailable` is also a "come back later" signal, and it may also carry `Retry-After`
//! — but it does **not** promise the request was not acted on. A gateway answers 503 when an
//! upstream became unreachable, which can happen *after* the request was forwarded; a server can
//! answer 503 part-way through handling one. Retrying that is the `Unreachable`-shaped uncertainty
//! the port already refuses to paper over, and this family carries requests whose method a model
//! chose, so no caller here can promise idempotence on its behalf. `flux-provider` does retry 5xx,
//! and the asymmetry is intended: its peer is one known completions endpoint whose calls the caller
//! already treats as replayable, not an arbitrary third-party API.
//!
//! Narrowing 503 to GET and HEAD would be defensible, and the door is left open — but a policy that
//! silently differs by method is a worse default than one a reader can state in a sentence, so it is
//! a separate decision rather than a hedge folded into this one.
//!
//! ## Determinism
//!
//! [`wait_after`] is a pure function of an [`Attempt`], jitter included. The only nondeterminism in
//! the module is [`jitter`], which reads the clock — so the schedule can be asserted exactly in a
//! unit test, and the live tests only ever have to bound a range that jitter can widen but not
//! unbound.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many times one guarded request may be retried after a 429. Conservative on purpose: the
/// point is to ride out a short rate-limit window, not to keep a turn alive against a server that
/// has decided to say no.
pub(crate) const MAX_RETRIES: u32 = 3;

/// Cap on a **single** wait. A `Retry-After` longer than this is not honored by waiting less — the
/// 429 goes back to the caller with its header intact, so an authored program can decide.
pub(crate) const MAX_SINGLE_WAIT: Duration = Duration::from_secs(30);

/// Cap on the **total** time one request may spend waiting across all of its retries.
pub(crate) const MAX_TOTAL_WAIT: Duration = Duration::from_secs(60);

/// The request budget a retry has to leave for the attempt it is about to make. Without it a wait
/// that exactly consumed the remaining budget would turn a perfectly good 429 — which is *data* —
/// into a timeout *error*, which is strictly worse for the caller.
pub(crate) const RETRY_HEADROOM: Duration = Duration::from_secs(1);

/// First step of the exponential backoff used when the server named no delay. Doubles per retry.
pub(crate) const BACKOFF_STEP: Duration = Duration::from_millis(500);

/// The width of the jitter added to every wait: `[0, JITTER_SPAN]`, always added and never
/// subtracted, so a server that named a delay is never come back to early.
pub(crate) const JITTER_SPAN: Duration = Duration::from_millis(250);

/// Everything one rate-limit decision is made from, gathered so that the decision itself
/// ([`wait_after`]) is a pure function a test can drive without a clock or a socket.
pub(crate) struct Attempt<'a> {
    /// The status this attempt answered with.
    pub status: u16,
    /// The answer's `Retry-After` header value, if it carried one.
    pub retry_after: Option<&'a str>,
    /// Retries already made for this request. Zero on the first answer.
    pub retries: u32,
    /// Time already spent waiting between this request's attempts.
    pub waited: Duration,
    /// What is left of the request's wall-clock budget, measured after this attempt answered.
    pub remaining: Duration,
    /// The wall clock an HTTP-date `Retry-After` is measured against.
    pub now: SystemTime,
    /// The jitter sample to add — supplied rather than read, which is what keeps the schedule
    /// deterministic under test.
    pub jitter: Duration,
}

/// Whether a status is one this backend retries. See the module docs for why 503 is not on the list.
pub(crate) fn is_rate_limited(status: u16) -> bool {
    status == 429
}

/// How long to wait before trying `attempt` again, or `None` to hand the answer back as it stands.
///
/// Every bound is checked here rather than at the call site, so "why did it stop retrying" has one
/// place to read: the status, the attempt cap, the total-wait cap, a `Retry-After` longer than this
/// backend will hold a turn for, and — last, because it is the one the story is really about — the
/// request's own wall-clock budget.
pub(crate) fn wait_after(attempt: &Attempt<'_>) -> Option<Duration> {
    if !is_rate_limited(attempt.status) || attempt.retries >= MAX_RETRIES {
        return None;
    }
    let hint = attempt
        .retry_after
        .and_then(|value| retry_after(value, attempt.now));
    let base = match hint {
        // Asked to wait longer than this backend will hold a turn. Retrying *earlier* than the
        // server said would be the one response worse than not retrying, so the 429 goes back.
        Some(hint) if hint > MAX_SINGLE_WAIT => return None,
        Some(hint) => hint,
        None => backoff(attempt.retries + 1),
    };
    let wait = base.saturating_add(attempt.jitter);
    if attempt.waited.saturating_add(wait) > MAX_TOTAL_WAIT {
        return None;
    }
    // The budget bounds the whole chain, waits included. A retry that cannot both wait and still
    // leave the next attempt room inside the budget returns the 429 rather than blocking past it.
    if wait.saturating_add(RETRY_HEADROOM) > attempt.remaining {
        return None;
    }
    Some(wait)
}

/// Exponential backoff for a 1-based retry number: [`BACKOFF_STEP`] doubled per retry, capped.
pub(crate) fn backoff(retry: u32) -> Duration {
    let step = BACKOFF_STEP.saturating_mul(1u32 << retry.saturating_sub(1).min(16));
    step.min(MAX_SINGLE_WAIT)
}

/// A bounded jitter sample in `[0, JITTER_SPAN]`, derived from the clock this process already has.
///
/// It exists so a fleet of callers rate-limited by the same service does not come back in lockstep
/// and re-create the burst. No randomness source is needed for that — sub-second clock skew between
/// processes is exactly the spread being asked for — and this way the module adds no dependency.
pub(crate) fn jitter() -> Duration {
    let span = JITTER_SPAN.as_millis() as u64 + 1;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| u64::from(since.subsec_nanos()))
        .unwrap_or(0);
    Duration::from_millis(nanos % span)
}

/// Parse a `Retry-After` value in **either** of its forms, as a delay from `now`.
///
/// RFC 9110 §10.2.3 states it as `delay-seconds / HTTP-date`, and a date already in the past means
/// "come back now" rather than "this header is broken" — so it clamps to zero instead of failing.
/// An unparseable value is `None`, which puts the caller on the exponential backoff.
pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = http_date(value)?;
    let now = i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(Duration::from_secs(at.saturating_sub(now).max(0) as u64))
}

/// Seconds since the Unix epoch for an `HTTP-date`, in all three spellings RFC 9110 §5.6.7 requires
/// a recipient to accept:
///
/// ```text
/// Sun, 06 Nov 1994 08:49:37 GMT    ; IMF-fixdate — the only one a sender may produce
/// Sunday, 06-Nov-94 08:49:37 GMT   ; obsolete RFC 850
/// Sun Nov  6 08:49:37 1994         ; obsolete asctime
/// ```
///
/// The day-of-week is parsed off and discarded: it is redundant with the date, and refusing to wait
/// because a server computed the wrong weekday would be a strange place to be strict. Every form is
/// GMT by definition, so no zone handling is involved.
fn http_date(value: &str) -> Option<i64> {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let (day, month, year, time) = match tokens.as_slice() {
        [_weekday, day, month, year, time, zone] if zone.eq_ignore_ascii_case("GMT") => (
            day.parse::<i64>().ok()?,
            month_of(month)?,
            year.parse::<i64>().ok()?,
            *time,
        ),
        [_weekday, date, time, zone] if zone.eq_ignore_ascii_case("GMT") => {
            let mut parts = date.split('-');
            let day = parts.next()?.parse::<i64>().ok()?;
            let month = month_of(parts.next()?)?;
            let year = two_digit_year(parts.next()?)?;
            if parts.next().is_some() {
                return None;
            }
            (day, month, year, *time)
        }
        [_weekday, month, day, time, year] => (
            day.parse::<i64>().ok()?,
            month_of(month)?,
            year.parse::<i64>().ok()?,
            *time,
        ),
        _ => return None,
    };
    let mut clock = time.split(':');
    let hour = clock.next()?.parse::<i64>().ok()?;
    let minute = clock.next()?.parse::<i64>().ok()?;
    let second = clock.next()?.parse::<i64>().ok()?;
    if clock.next().is_some() || !(1..=31).contains(&day) {
        return None;
    }
    // A leap second is `:60`, and a date this backend only ever subtracts `now` from does not need
    // to be precious about it.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// 1-based month for an English three-letter abbreviation, which is all any HTTP-date form uses.
fn month_of(name: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let name = name.get(..3)?.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|month| *month == name)
        .map(|index| index as i64 + 1)
}

/// The obsolete RFC 850 form carries a two-digit year. RFC 9110 asks a recipient to read one that
/// lands more than fifty years ahead as the most recent past year with the same last two digits;
/// the POSIX pivot below agrees with that rule for every `Retry-After` anyone would actually send,
/// because a "come back later" date is minutes away and not decades.
fn two_digit_year(year: &str) -> Option<i64> {
    let year = year.parse::<i64>().ok()?;
    if !(0..=99).contains(&year) {
        return None;
    }
    Some(if year < 70 {
        year + 2_000
    } else {
        year + 1_900
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`), the
/// same algorithm `flux-capabilities` already carries — this crate reaches for it rather than a date
/// dependency for the same reason that one does.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2015-10-21T07:28:00Z — the date every HTTP-date example in the RFCs is written around.
    const REFERENCE: u64 = 1_445_412_480;

    fn at(unix: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix)
    }

    fn attempt<'a>(status: u16, retry_after: Option<&'a str>) -> Attempt<'a> {
        Attempt {
            status,
            retry_after,
            retries: 0,
            waited: Duration::ZERO,
            remaining: Duration::from_secs(300),
            now: at(REFERENCE),
            jitter: Duration::ZERO,
        }
    }

    /// The status gate: 429 and nothing else. 503 is the one that has to stay off, because it does
    /// not promise the request went unacted-on — see the module docs.
    #[test]
    fn only_a_429_is_retried() {
        assert!(is_rate_limited(429));
        for status in [200, 400, 403, 404, 500, 502, 503, 504] {
            assert!(!is_rate_limited(status), "{status} must not be retried");
        }
        assert!(
            wait_after(&attempt(503, Some("1"))).is_none(),
            "a 503 with a Retry-After is still not retried"
        );
    }

    /// `Retry-After` in its delta-seconds form.
    #[test]
    fn delta_seconds_is_read_as_a_delay() {
        assert_eq!(retry_after("0", at(REFERENCE)), Some(Duration::ZERO));
        assert_eq!(
            retry_after("7", at(REFERENCE)),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after("  120  ", at(REFERENCE)),
            Some(Duration::from_secs(120))
        );
    }

    /// `Retry-After` in its HTTP-date form — all three spellings a recipient must accept, each
    /// naming the same instant, ninety seconds after the reference clock.
    #[test]
    fn every_http_date_spelling_is_read_as_the_same_delay() {
        let ninety_seconds_on = at(REFERENCE - 90);
        for spelling in [
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "Wednesday, 21-Oct-15 07:28:00 GMT",
            "Wed Oct 21 07:28:00 2015",
            // asctime pads a single-digit day with a second space, which `split_whitespace` folds.
            "Wed Oct  8 07:28:00 2015",
        ] {
            let parsed = retry_after(spelling, ninety_seconds_on)
                .unwrap_or_else(|| panic!("`{spelling}` is an HTTP-date and must parse"));
            let expected = if spelling.contains(" 8 ") {
                // The 8th is thirteen days before the 21st; the point is only that the day and
                // month were read, not that this particular arithmetic is interesting.
                Duration::ZERO
            } else {
                Duration::from_secs(90)
            };
            assert_eq!(parsed, expected, "`{spelling}`");
        }
    }

    /// A date already past means "come back now", not "this header is broken".
    #[test]
    fn an_http_date_in_the_past_is_no_wait_at_all() {
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:28:00 GMT", at(REFERENCE + 3_600)),
            Some(Duration::ZERO)
        );
    }

    /// Anything that is neither form leaves the caller on the backoff rather than inventing a delay.
    #[test]
    fn an_unusable_retry_after_is_no_hint() {
        for value in [
            "",
            "soon",
            "-5",
            "1.5",
            "Wed, 21 Oct 2015 07:28:00 CET",
            "Wed, 21 Fbz 2015 07:28:00 GMT",
            "Wed, 32 Oct 2015 07:28:00 GMT",
            "Wed, 21 Oct 2015 25:28:00 GMT",
            "Wed, 21 Oct 2015 07:28 GMT",
        ] {
            assert_eq!(retry_after(value, at(REFERENCE)), None, "`{value}`");
        }
        // …and an unusable header still leaves the request retryable, on the backoff.
        assert_eq!(
            wait_after(&attempt(429, Some("soon"))),
            Some(BACKOFF_STEP),
            "an unreadable Retry-After falls back to the backoff, it does not stop the retry"
        );
    }

    /// The headerless schedule: the backoff doubles per retry and is capped.
    #[test]
    fn the_backoff_doubles_and_is_capped() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(20), MAX_SINGLE_WAIT);
        let mut headerless = attempt(429, None);
        headerless.retries = 2;
        assert_eq!(wait_after(&headerless), Some(Duration::from_secs(2)));
    }

    /// Jitter is added, never subtracted: a server that named a delay is never come back to early.
    #[test]
    fn jitter_only_ever_lengthens_a_wait() {
        let mut jittered = attempt(429, Some("2"));
        jittered.jitter = Duration::from_millis(250);
        assert_eq!(
            wait_after(&jittered),
            Some(Duration::from_millis(2_250)),
            "the server's delay is a floor"
        );
        for _ in 0..64 {
            assert!(jitter() <= JITTER_SPAN, "the jitter sample stays bounded");
        }
    }

    /// The three caps that are not the request's budget: attempts, total wait, and a single wait
    /// longer than this backend will hold a turn for.
    #[test]
    fn the_attempt_and_wait_caps_stop_the_chain() {
        let mut spent = attempt(429, Some("1"));
        spent.retries = MAX_RETRIES;
        assert_eq!(wait_after(&spent), None, "the attempt cap holds");

        let mut long_waited = attempt(429, Some("30"));
        long_waited.retries = 1;
        long_waited.waited = MAX_TOTAL_WAIT - Duration::from_secs(10);
        assert_eq!(wait_after(&long_waited), None, "the total-wait cap holds");

        assert_eq!(
            wait_after(&attempt(429, Some("31"))),
            None,
            "a wait longer than this backend holds a turn returns the 429 rather than retrying early"
        );
        assert_eq!(
            wait_after(&attempt(429, Some("30"))),
            Some(Duration::from_secs(30)),
            "and exactly at the cap it is still honored"
        );
    }

    /// The request's wall-clock budget bounds the whole chain, waits included.
    #[test]
    fn the_request_budget_is_the_last_word() {
        let mut tight = attempt(429, Some("10"));
        tight.remaining = Duration::from_secs(10);
        assert_eq!(
            wait_after(&tight),
            None,
            "a wait that leaves the next attempt no room returns the 429"
        );

        let mut roomy = attempt(429, Some("10"));
        roomy.remaining = Duration::from_secs(10) + RETRY_HEADROOM;
        assert_eq!(
            wait_after(&roomy),
            Some(Duration::from_secs(10)),
            "with room for the wait and the attempt after it, the retry happens"
        );

        let mut exhausted = attempt(429, Some("0"));
        exhausted.remaining = Duration::ZERO;
        assert_eq!(
            wait_after(&exhausted),
            None,
            "an exhausted budget never waits"
        );
    }
}
