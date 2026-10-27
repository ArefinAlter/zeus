//! Queue schedules and the speed-limiter window: pure rules over a time zone and an instant.
//!
//! The functions follow the rules the client documents in `apps/client/src/model/schedule.ts` and take the same test
//! cases: times are wall-clock `HH:MM`, days are 0 (Sunday) to 6, a stop or window end earlier than its start runs
//! past midnight, and a window past midnight belongs to the day it started on. Instants are milliseconds since the
//! Unix epoch. The zone is a parameter, so the daemon can follow the computer's local time or UTC and the tests can
//! use a fixed offset.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike};
use zeus_api::methods::queues::{LimiterDef, QueueSchedule, Repeat, Timezone};

const MINUTE_MS: i64 = 60_000;

/// Minutes after midnight for `HH:MM` (a one-digit hour is accepted), or `None` when the text is not a time.
pub fn parse_hm(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    if !h.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// `YYYY-MM-DD` to a calendar day.
pub fn parse_ymd(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

/// Whether `days` holds only weekdays 0 to 6, each once.
pub fn valid_days(days: &[u8]) -> bool {
    let mut seen = [false; 7];
    days.iter().all(|&d| {
        let ok = d < 7 && !seen[d as usize];
        if let Some(slot) = seen.get_mut(d as usize) {
            *slot = true;
        }
        ok
    })
}

/// The instant of wall-clock `minutes` on `day`. A time that does not exist (a clock change skips it) moves forward
/// to the first instant that does; a time that happens twice takes the first.
fn at<Tz: TimeZone>(tz: &Tz, day: NaiveDate, minutes: u32) -> Option<i64> {
    let naive: NaiveDateTime = day.and_hms_opt(minutes / 60, minutes % 60, 0)?;
    for skip in 0..=3 {
        let candidate = naive + Duration::hours(skip);
        if let Some(t) = tz.from_local_datetime(&candidate).earliest() {
            return Some(t.timestamp_millis());
        }
    }
    None
}

/// The calendar day containing `ms` in `tz`.
fn day_of<Tz: TimeZone>(tz: &Tz, ms: i64) -> Option<NaiveDate> {
    Some(tz.timestamp_millis_opt(ms).single()?.date_naive())
}

fn weekday(day: NaiveDate) -> u8 {
    // `num_days_from_sunday` is 0 to 6.
    day.weekday().num_days_from_sunday() as u8
}

/// Offset of `tz` from UTC at `ms`, in minutes.
pub fn offset_minutes<Tz: TimeZone>(tz: &Tz, ms: i64) -> i32 {
    tz.timestamp_millis_opt(ms)
        .single()
        .map_or(0, |t| t.offset().fix().local_minus_utc() / 60)
}

/// The next start of a schedule after `now_ms` (at or after it with `inclusive`), or `None` when the schedule has
/// none: start disabled, a bad time, a one-time date in the past, or a repeating schedule with no day selected.
pub fn next_start<Tz: TimeZone>(
    tz: &Tz,
    schedule: &QueueSchedule,
    now_ms: i64,
    inclusive: bool,
) -> Option<i64> {
    if !schedule.start_enabled {
        return None;
    }
    let minutes = parse_hm(&schedule.start_time)?;
    let after = |t: i64| if inclusive { t >= now_ms } else { t > now_ms };
    if schedule.repeat == Repeat::Once {
        let t = at(tz, parse_ymd(&schedule.start_date)?, minutes)?;
        return after(t).then_some(t);
    }
    if schedule.days.is_empty() {
        return None;
    }
    let today = day_of(tz, now_ms)?;
    for offset in 0..=7 {
        let day = today + Duration::days(offset);
        if !schedule.days.contains(&weekday(day)) {
            continue;
        }
        let t = at(tz, day, minutes)?;
        if after(t) {
            return Some(t);
        }
    }
    None
}

/// The stop that follows a start at `started_ms`, or `None` when the schedule has no stop time.
pub fn stop_after<Tz: TimeZone>(tz: &Tz, schedule: &QueueSchedule, started_ms: i64) -> Option<i64> {
    if !schedule.stop_enabled {
        return None;
    }
    let stop = parse_hm(&schedule.stop_time)?;
    let day = day_of(tz, started_ms)?;
    let t = at(tz, day, stop)?;
    if t > started_ms {
        Some(t)
    } else {
        at(tz, day + Duration::days(1), stop)
    }
}

/// Whether a start fell due in `(prev_ms, now_ms]`.
pub fn start_due<Tz: TimeZone>(
    tz: &Tz,
    schedule: &QueueSchedule,
    prev_ms: i64,
    now_ms: i64,
) -> bool {
    next_start(tz, schedule, prev_ms, false).is_some_and(|t| t <= now_ms)
}

/// Whether the scheduled stop of a run that began at `started_ms` fell due in `(prev_ms, now_ms]`.
pub fn stop_due<Tz: TimeZone>(
    tz: &Tz,
    schedule: &QueueSchedule,
    started_ms: Option<i64>,
    prev_ms: i64,
    now_ms: i64,
) -> bool {
    started_ms
        .and_then(|s| stop_after(tz, schedule, s))
        .is_some_and(|t| t > prev_ms && t <= now_ms)
}

/// The latest scheduled start at or before `now_ms` and after `since_ms`, for a start missed while the daemon was
/// off. Looks back at most eight days.
pub fn latest_start_in<Tz: TimeZone>(
    tz: &Tz,
    schedule: &QueueSchedule,
    since_ms: i64,
    now_ms: i64,
) -> Option<i64> {
    if now_ms <= since_ms {
        return None;
    }
    let mut found = None;
    let mut cursor = since_ms;
    for _ in 0..16 {
        match next_start(tz, schedule, cursor, false) {
            Some(t) if t <= now_ms => {
                found = Some(t);
                cursor = t;
            }
            _ => break,
        }
    }
    found
}

/// Whether `now_ms` lies inside the daily window `[from, to)` on one of `days`; a window past midnight belongs to
/// its start day. Equal times mean the whole of each listed day.
pub fn in_window<Tz: TimeZone>(tz: &Tz, from: &str, to: &str, days: &[u8], now_ms: i64) -> bool {
    let (Some(f), Some(t)) = (parse_hm(from), parse_hm(to)) else {
        return false;
    };
    if days.is_empty() {
        return false;
    }
    let Some(now) = tz.timestamp_millis_opt(now_ms).single() else {
        return false;
    };
    let minutes = now.hour() * 60 + now.minute();
    let today = weekday(now.date_naive());
    let yesterday = (today + 6) % 7;
    if f == t {
        return days.contains(&today);
    }
    if f < t {
        return days.contains(&today) && minutes >= f && minutes < t;
    }
    (days.contains(&today) && minutes >= f) || (days.contains(&yesterday) && minutes < t)
}

/// Whether the global limit applies at `now_ms`.
pub fn limiter_active<Tz: TimeZone>(tz: &Tz, limiter: &LimiterDef, now_ms: i64) -> bool {
    if !limiter.enabled {
        return false;
    }
    if !limiter.schedule_enabled {
        return true;
    }
    in_window(tz, &limiter.from, &limiter.to, &limiter.days, now_ms)
}

/// The next instant after `now_ms` at which [`limiter_active`] may change (a window edge), within eight days.
pub fn next_limiter_edge<Tz: TimeZone>(tz: &Tz, limiter: &LimiterDef, now_ms: i64) -> Option<i64> {
    if !limiter.enabled || !limiter.schedule_enabled {
        return None;
    }
    let state = limiter_active(tz, limiter, now_ms);
    // Edges fall on whole minutes: scan minute by minute for at most eight days is too slow, so jump by the
    // candidate edges of the window itself.
    let (f, t) = (parse_hm(&limiter.from)?, parse_hm(&limiter.to)?);
    let today = day_of(tz, now_ms)?;
    let mut best: Option<i64> = None;
    for offset in 0..=8 {
        let day = today + Duration::days(offset);
        for minutes in [f, t] {
            if let Some(edge) = at(tz, day, minutes)
                && edge > now_ms
                && limiter_active(tz, limiter, edge) != state
                && best.is_none_or(|b| edge < b)
            {
                best = Some(edge);
            }
        }
    }
    best
}

/// The clock the daemon follows, chosen by the `timezone` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    zone: Timezone,
}

impl Clock {
    /// A clock for `zone`.
    pub const fn new(zone: Timezone) -> Self {
        Self { zone }
    }

    /// The configured zone.
    pub const fn zone(self) -> Timezone {
        self.zone
    }

    /// [`next_start`].
    pub fn next_start(self, s: &QueueSchedule, now_ms: i64, inclusive: bool) -> Option<i64> {
        match self.zone {
            Timezone::Local => next_start(&chrono::Local, s, now_ms, inclusive),
            Timezone::Utc => next_start(&chrono::Utc, s, now_ms, inclusive),
        }
    }

    /// [`stop_after`].
    pub fn stop_after(self, s: &QueueSchedule, started_ms: i64) -> Option<i64> {
        match self.zone {
            Timezone::Local => stop_after(&chrono::Local, s, started_ms),
            Timezone::Utc => stop_after(&chrono::Utc, s, started_ms),
        }
    }

    /// [`start_due`].
    pub fn start_due(self, s: &QueueSchedule, prev_ms: i64, now_ms: i64) -> bool {
        match self.zone {
            Timezone::Local => start_due(&chrono::Local, s, prev_ms, now_ms),
            Timezone::Utc => start_due(&chrono::Utc, s, prev_ms, now_ms),
        }
    }

    /// [`stop_due`].
    pub fn stop_due(
        self,
        s: &QueueSchedule,
        started_ms: Option<i64>,
        prev_ms: i64,
        now_ms: i64,
    ) -> bool {
        match self.zone {
            Timezone::Local => stop_due(&chrono::Local, s, started_ms, prev_ms, now_ms),
            Timezone::Utc => stop_due(&chrono::Utc, s, started_ms, prev_ms, now_ms),
        }
    }

    /// [`latest_start_in`].
    pub fn latest_start_in(self, s: &QueueSchedule, since_ms: i64, now_ms: i64) -> Option<i64> {
        match self.zone {
            Timezone::Local => latest_start_in(&chrono::Local, s, since_ms, now_ms),
            Timezone::Utc => latest_start_in(&chrono::Utc, s, since_ms, now_ms),
        }
    }

    /// [`limiter_active`].
    pub fn limiter_active(self, l: &LimiterDef, now_ms: i64) -> bool {
        match self.zone {
            Timezone::Local => limiter_active(&chrono::Local, l, now_ms),
            Timezone::Utc => limiter_active(&chrono::Utc, l, now_ms),
        }
    }

    /// [`offset_minutes`].
    pub fn offset_minutes(self, now_ms: i64) -> i32 {
        match self.zone {
            Timezone::Local => offset_minutes(&chrono::Local, now_ms),
            Timezone::Utc => 0,
        }
    }
}

/// Milliseconds in a minute, for callers that round to schedule granularity.
pub const MINUTE: i64 = MINUTE_MS;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    // The client's test clock is Sunday 25 October 2026, 14:00 local. Use UTC+2 so the cases do not depend on
    // the machine's zone.
    fn zone() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        zone()
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn sched() -> QueueSchedule {
        QueueSchedule {
            start_enabled: true,
            start_time: "23:00".into(),
            repeat: Repeat::Days,
            start_date: "2026-10-25".into(),
            days: vec![0, 1, 2, 3, 4, 5, 6],
            stop_enabled: true,
            stop_time: "06:00".into(),
        }
    }

    fn now() -> i64 {
        ms(2026, 10, 25, 14, 0)
    }

    #[test]
    fn a_daily_start_is_today_when_still_ahead_else_tomorrow() {
        assert_eq!(
            next_start(&zone(), &sched(), now(), false),
            Some(ms(2026, 10, 25, 23, 0))
        );
        let late = ms(2026, 10, 25, 23, 30);
        assert_eq!(
            next_start(&zone(), &sched(), late, false),
            Some(ms(2026, 10, 26, 23, 0))
        );
    }

    #[test]
    fn weekday_schedules_skip_the_weekend_and_one_time_starts_in_the_past_have_no_next_start() {
        let weekdays = QueueSchedule {
            days: vec![1, 2, 3, 4, 5],
            start_time: "02:30".into(),
            ..sched()
        };
        assert_eq!(
            next_start(&zone(), &weekdays, now(), false),
            Some(ms(2026, 10, 26, 2, 30))
        );
        let once = |date: &str, time: &str| QueueSchedule {
            repeat: Repeat::Once,
            start_date: date.into(),
            start_time: time.into(),
            ..sched()
        };
        assert_eq!(
            next_start(&zone(), &once("2026-10-24", "23:00"), now(), false),
            None
        );
        assert_eq!(
            next_start(&zone(), &once("2026-10-26", "01:00"), now(), false),
            Some(ms(2026, 10, 26, 1, 0))
        );
        let none = QueueSchedule {
            days: vec![],
            ..sched()
        };
        assert_eq!(next_start(&zone(), &none, now(), false), None);
        let off = QueueSchedule {
            start_enabled: false,
            ..sched()
        };
        assert_eq!(next_start(&zone(), &off, now(), false), None);
    }

    #[test]
    fn a_stop_earlier_than_the_start_falls_on_the_next_day_and_due_checks_are_half_open() {
        let start = ms(2026, 10, 25, 23, 0);
        assert_eq!(
            stop_after(&zone(), &sched(), start),
            Some(ms(2026, 10, 26, 6, 0))
        );
        assert!(start_due(&zone(), &sched(), start - 1000, start));
        assert!(!start_due(&zone(), &sched(), start, start + 1000));
        assert!(stop_due(
            &zone(),
            &sched(),
            Some(start),
            ms(2026, 10, 26, 5, 59),
            ms(2026, 10, 26, 6, 0)
        ));
        assert!(!stop_due(
            &zone(),
            &sched(),
            None,
            ms(2026, 10, 26, 5, 59),
            ms(2026, 10, 26, 6, 0)
        ));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("7:05"), Some(425));
        assert_eq!(parse_hm("07:5"), None);
        assert_eq!(parse_hm("noon"), None);
    }

    #[test]
    fn windows_past_midnight_belong_to_their_start_day() {
        let sat = 6;
        let late_sat = ms(2026, 10, 24, 23, 30);
        let early_sun = ms(2026, 10, 25, 5, 0);
        let sun_noon = ms(2026, 10, 25, 12, 0);
        assert!(in_window(&zone(), "22:00", "07:00", &[sat], late_sat));
        assert!(in_window(&zone(), "22:00", "07:00", &[sat], early_sun));
        assert!(!in_window(&zone(), "22:00", "07:00", &[sat], sun_noon));
        assert!(in_window(&zone(), "09:00", "17:00", &[0], sun_noon));
        let limiter = LimiterDef {
            enabled: true,
            limit_kib_s: 1024,
            schedule_enabled: true,
            from: "09:00".into(),
            to: "17:00".into(),
            days: vec![1, 2, 3, 4, 5],
        };
        assert!(!limiter_active(&zone(), &limiter, sun_noon));
        let always = LimiterDef {
            schedule_enabled: false,
            ..limiter.clone()
        };
        assert!(limiter_active(&zone(), &always, sun_noon));
        let off = LimiterDef {
            enabled: false,
            ..limiter
        };
        assert!(!limiter_active(&zone(), &off, sun_noon));
    }

    #[test]
    fn the_limiter_edge_is_the_next_time_the_window_opens_or_closes() {
        let limiter = LimiterDef {
            enabled: true,
            limit_kib_s: 100,
            schedule_enabled: true,
            from: "09:00".into(),
            to: "17:00".into(),
            days: vec![1, 2, 3, 4, 5],
        };
        // Sunday noon: the window opens Monday 09:00.
        assert_eq!(
            next_limiter_edge(&zone(), &limiter, ms(2026, 10, 25, 12, 0)),
            Some(ms(2026, 10, 26, 9, 0))
        );
        // Monday 10:00: it closes at 17:00.
        assert_eq!(
            next_limiter_edge(&zone(), &limiter, ms(2026, 10, 26, 10, 0)),
            Some(ms(2026, 10, 26, 17, 0))
        );
        let flat = LimiterDef {
            schedule_enabled: false,
            ..limiter
        };
        assert_eq!(
            next_limiter_edge(&zone(), &flat, ms(2026, 10, 26, 10, 0)),
            None
        );
    }

    #[test]
    fn a_missed_start_is_found_for_run_once() {
        // Started daily at 23:00; the daemon was off from Saturday evening to Sunday 14:00.
        let since = ms(2026, 10, 24, 22, 0);
        assert_eq!(
            latest_start_in(&zone(), &sched(), since, now()),
            Some(ms(2026, 10, 24, 23, 0))
        );
        assert_eq!(
            latest_start_in(&zone(), &sched(), ms(2026, 10, 24, 23, 0), now()),
            None
        );
    }

    #[test]
    fn day_validation_and_utc_clock() {
        assert!(valid_days(&[0, 6, 3]));
        assert!(!valid_days(&[7]));
        assert!(!valid_days(&[1, 1]));
        let clock = Clock::new(Timezone::Utc);
        let utc_noon = chrono::Utc
            .with_ymd_and_hms(2026, 10, 25, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        let s = QueueSchedule {
            start_time: "13:00".into(),
            ..sched()
        };
        assert_eq!(
            clock.next_start(&s, utc_noon, false),
            Some(utc_noon + 60 * 60_000)
        );
        assert_eq!(clock.offset_minutes(utc_noon), 0);
    }
}
