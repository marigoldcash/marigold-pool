use crate::{PayoutSettings, run_cycle};
use chrono::{Duration as ChronoDuration, NaiveDateTime, Timelike};
use pool_chain::SharedPayer;
use pool_db::Db;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// When the payout cycle runs, written the way cron writes the first two fields:
/// `minute hour * * *`. The minute is a number, the hour is a number or `*` for
/// every hour, and the other three fields must be `*`. So `0 5 * * *` is every
/// day at 05:00 and `30 * * * *` is half past every hour. Times are UTC, whatever
/// the machine's own clock says, so the pool pays at the same moment everywhere
/// and nothing shifts with the clocks changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    minute: u32,
    hour: Option<u32>,
}

impl Schedule {
    pub fn parse(text: &str) -> Result<Schedule, String> {
        let wrong = || {
            format!(
                "The payout schedule {text:?} is not something the pool can read. Write it as \"minute hour * * *\", for example \"0 5 * * *\" for every day at 05:00."
            )
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 5 || fields[2..].iter().any(|f| *f != "*") {
            return Err(wrong());
        }
        let minute: u32 = fields[0].parse().map_err(|_| wrong())?;
        let hour = if fields[1] == "*" { None } else { Some(fields[1].parse::<u32>().map_err(|_| wrong())?) };
        if minute > 59 || hour.is_some_and(|h| h > 23) {
            return Err(wrong());
        }
        Ok(Schedule { minute, hour })
    }

    /// The schedule in words, for people: `every day at 05:00 UTC`.
    pub fn describe(&self) -> String {
        match (self.hour, self.minute) {
            (Some(hour), minute) => format!("every day at {hour:02}:{minute:02} UTC"),
            (None, 0) => "every hour, on the hour (UTC)".to_owned(),
            (None, minute) => format!("every hour, at {minute} minutes past the hour (UTC)"),
        }
    }

    /// The first run after `now_ms` (milliseconds since the Unix epoch, UTC), as
    /// milliseconds since the Unix epoch.
    pub fn next_run_ms(&self, now_ms: u64) -> u64 {
        let now = chrono::DateTime::from_timestamp_millis(i64::try_from(now_ms).unwrap_or(i64::MAX))
            .map(|t| t.naive_utc())
            .unwrap_or_default();
        u64::try_from(self.next_after(now).and_utc().timestamp_millis()).unwrap_or(0)
    }

    /// The first time strictly after `after` that the schedule fires.
    pub fn next_after(&self, after: NaiveDateTime) -> NaiveDateTime {
        match self.hour {
            Some(hour) => {
                let today = after.date().and_hms_opt(hour, self.minute, 0).expect("hour and minute were checked");
                if today > after { today } else { today + ChronoDuration::days(1) }
            }
            None => {
                let this_hour = after.date().and_hms_opt(after.hour(), self.minute, 0).expect("minute was checked");
                if this_hour > after { this_hour } else { this_hour + ChronoDuration::hours(1) }
            }
        }
    }
}

/// Run the payout cycle at every time the schedule names, until `shutdown`
/// changes.
pub async fn run_scheduler(
    db: Db,
    payer: SharedPayer,
    settings: PayoutSettings,
    schedule: Schedule,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let now = chrono::Utc::now().naive_utc();
        let next = schedule.next_after(now);
        let wait = (next - now).to_std().unwrap_or(Duration::from_secs(60));
        info!("the next payout run is at {next} (in {} minutes)", wait.as_secs() / 60);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        match run_cycle(&db, payer.as_ref(), &settings, now_ms).await {
            Ok(summary) => info!("payout run finished: {summary:?}"),
            Err(crate::PayoutError::Busy) => warn!("a payout run is already going; this one was skipped"),
            Err(why) => error!("the payout run did not finish: {why}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, s).unwrap()
    }

    #[test]
    fn a_daily_schedule_fires_at_the_next_matching_time() {
        let daily = Schedule::parse("0 5 * * *").unwrap();
        assert_eq!(daily.next_after(at(2026, 10, 12, 4, 59, 59)), at(2026, 10, 12, 5, 0, 0), "later today");
        assert_eq!(daily.next_after(at(2026, 10, 12, 5, 0, 0)), at(2026, 10, 13, 5, 0, 0), "exactly now means tomorrow");
        assert_eq!(daily.next_after(at(2026, 10, 12, 17, 30, 0)), at(2026, 10, 13, 5, 0, 0));
        assert_eq!(daily.next_after(at(2026, 12, 31, 23, 0, 0)), at(2027, 1, 1, 5, 0, 0), "across a year end");
    }

    #[test]
    fn schedules_describe_themselves_in_words_and_say_utc() {
        assert_eq!(Schedule::parse("0 5 * * *").unwrap().describe(), "every day at 05:00 UTC");
        assert_eq!(Schedule::parse("30 14 * * *").unwrap().describe(), "every day at 14:30 UTC");
        assert_eq!(Schedule::parse("0 * * * *").unwrap().describe(), "every hour, on the hour (UTC)");
        assert_eq!(Schedule::parse("15 * * * *").unwrap().describe(), "every hour, at 15 minutes past the hour (UTC)");
    }

    #[test]
    fn the_next_run_as_a_time_is_in_utc() {
        let daily = Schedule::parse("0 5 * * *").unwrap();
        // 1,000,000,000,000 ms is 2001-09-09 01:46:40 UTC; the next 05:00 is the same day.
        assert_eq!(daily.next_run_ms(1_000_000_000_000), 1_000_011_600_000);
        // At exactly 05:00 the next run is tomorrow's.
        assert_eq!(daily.next_run_ms(1_000_011_600_000), 1_000_011_600_000 + 86_400_000);
    }

    #[test]
    fn an_hourly_schedule_fires_at_the_minute_every_hour() {
        let hourly = Schedule::parse("30 * * * *").unwrap();
        assert_eq!(hourly.next_after(at(2026, 10, 12, 4, 10, 0)), at(2026, 10, 12, 4, 30, 0));
        assert_eq!(hourly.next_after(at(2026, 10, 12, 4, 30, 0)), at(2026, 10, 12, 5, 30, 0));
        assert_eq!(hourly.next_after(at(2026, 10, 12, 23, 45, 0)), at(2026, 10, 13, 0, 30, 0), "across midnight");
    }

    #[test]
    fn anything_else_is_refused_with_an_example() {
        for bad in
            ["", "0 5", "0 5 * *", "0 5 * * 1", "0 5 1 * *", "*/5 * * * *", "60 5 * * *", "0 24 * * *", "x 5 * * *", "0 5 * * * *"]
        {
            let error = Schedule::parse(bad).unwrap_err();
            assert!(error.contains("0 5 * * *") && error.contains("minute hour"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn extra_spaces_do_not_matter() {
        assert_eq!(Schedule::parse("  0   5  *  *  * ").unwrap(), Schedule::parse("0 5 * * *").unwrap());
    }
}
