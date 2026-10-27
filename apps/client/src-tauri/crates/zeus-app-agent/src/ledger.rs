//! The spend ledger: tokens and estimated cost per day and provider, and per thread, with caps.
//!
//! Amounts are whole micro-dollars (one token at one dollar per million tokens costs one micro-dollar), so there is no
//! floating point in the sums. Prices come from the person's price table; a provider without prices uses the default
//! rate, which is deliberately on the high side so a cap trips early rather than late. A cap of zero means no call.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{AgentConfig, ProviderCfg};
use crate::provider::Usage;
use zeus_app::engine_config::write_atomic;
use zeus_app::home::AppHome;

/// Totals of one bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Prompt tokens.
    pub input: u64,
    /// Completion tokens.
    pub output: u64,
    /// Estimated cost in micro-dollars.
    pub micro_usd: u64,
    /// Calls.
    pub calls: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    /// `YYYY-MM-DD` (UTC) to provider id to totals.
    days: BTreeMap<String, BTreeMap<String, Totals>>,
    /// Thread id to totals.
    threads: BTreeMap<String, Totals>,
}

/// The ledger.
pub struct Ledger {
    path: PathBuf,
    data: Mutex<Data>,
}

/// Why a call was refused before it was made.
#[derive(Debug, Clone, PartialEq)]
pub struct CapRefusal {
    /// Which cap: `provider`, `daily` or `thread`.
    pub scope: &'static str,
    /// The cap in US dollars.
    pub cap_usd: f64,
    /// Spent so far in US dollars.
    pub spent_usd: f64,
}

impl CapRefusal {
    /// A sentence for the thread.
    pub fn message(&self) -> String {
        let what = match self.scope {
            "provider" => "this provider's monthly cap",
            "daily" => "the daily cap",
            _ => "this thread's cap",
        };
        if self.cap_usd <= 0.0 {
            format!("No model call was made: {what} is zero. Raise it in Settings, Providers.")
        } else {
            format!(
                "No model call was made: {what} of ${:.2} would be passed (${:.2} spent). Raise it in Settings, Providers.",
                self.cap_usd, self.spent_usd
            )
        }
    }
}

/// Days since the Unix epoch to `(year, month, day)`.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The UTC date of `ms` since the epoch as `YYYY-MM-DD`.
pub fn day_key(ms: u64) -> String {
    let (y, m, d) = civil_from_days((ms / 86_400_000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

fn usd(micro: u64) -> f64 {
    micro as f64 / 1_000_000.0
}

fn micro_from_usd(v: f64) -> u64 {
    (v.max(0.0) * 1_000_000.0).round() as u64
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The cost of a usage at the given prices, in micro-dollars.
pub fn cost_micro(usage: Usage, price_in: f64, price_out: f64) -> u64 {
    (usage.input as f64 * price_in + usage.output as f64 * price_out).round() as u64
}

impl Ledger {
    /// The ledger under `home`.
    pub fn new(home: &AppHome) -> Self {
        let path = home.app_dir().join("agent").join("ledger.json");
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    fn save(&self, data: &Data) {
        if let Ok(b) = serde_json::to_vec(data) {
            let _ = write_atomic(&self.path, &b);
        }
    }

    /// Record a finished call.
    pub fn charge(&self, provider: &str, thread: &str, usage: Usage, micro: u64) {
        let Ok(mut d) = self.data.lock() else { return };
        let add = |t: &mut Totals| {
            t.input += usage.input;
            t.output += usage.output;
            t.micro_usd += micro;
            t.calls += 1;
        };
        add(d
            .days
            .entry(day_key(now_ms()))
            .or_default()
            .entry(provider.to_owned())
            .or_default());
        add(d.threads.entry(thread.to_owned()).or_default());
        while d.days.len() > 400 {
            if let Some(k) = d.days.keys().next().cloned() {
                d.days.remove(&k);
            }
        }
        if d.threads.len() > 2000
            && let Some(k) = d.threads.keys().next().cloned()
        {
            d.threads.remove(&k);
        }
        self.save(&d);
    }

    /// Totals of a thread.
    pub fn thread(&self, id: &str) -> Totals {
        self.data
            .lock()
            .ok()
            .and_then(|d| d.threads.get(id).copied())
            .unwrap_or_default()
    }

    /// Forget a thread's totals (the thread was deleted).
    pub fn forget_thread(&self, id: &str) {
        if let Ok(mut d) = self.data.lock() {
            d.threads.remove(id);
            self.save(&d);
        }
    }

    /// Everything spent today over all providers.
    pub fn today(&self) -> Totals {
        let key = day_key(now_ms());
        self.sum(|day, _| day == key)
    }

    /// Everything spent this calendar month (UTC) by one provider.
    pub fn month(&self, provider: &str) -> Totals {
        let month = day_key(now_ms())[..7].to_owned();
        let provider = provider.to_owned();
        self.sum(move |day, p| day.starts_with(&month) && p == provider)
    }

    fn sum(&self, keep: impl Fn(&str, &str) -> bool) -> Totals {
        let Ok(d) = self.data.lock() else {
            return Totals::default();
        };
        let mut t = Totals::default();
        for (day, per) in &d.days {
            for (p, v) in per {
                if keep(day, p) {
                    t.input += v.input;
                    t.output += v.output;
                    t.micro_usd += v.micro_usd;
                    t.calls += v.calls;
                }
            }
        }
        t
    }

    /// Check the caps before a call. `estimated_input` is the prompt's token estimate.
    pub fn check(
        &self,
        config: &AgentConfig,
        provider: &ProviderCfg,
        thread: &str,
        estimated_input: u64,
    ) -> Result<(), CapRefusal> {
        let (pi, po) = config.prices(provider);
        let est = cost_micro(
            Usage {
                input: estimated_input,
                output: 0,
                estimated: true,
            },
            pi,
            po,
        );
        let _ = po;
        let month = self.month(&provider.id).micro_usd;
        let cap_p = micro_from_usd(provider.monthly_cap_usd);
        if provider.monthly_cap_usd <= 0.0 || month + est > cap_p {
            return Err(CapRefusal {
                scope: "provider",
                cap_usd: provider.monthly_cap_usd,
                spent_usd: usd(month),
            });
        }
        let day = self.today().micro_usd;
        let cap_d = micro_from_usd(config.daily_cap_usd);
        if config.daily_cap_usd <= 0.0 || day + est > cap_d {
            return Err(CapRefusal {
                scope: "daily",
                cap_usd: config.daily_cap_usd,
                spent_usd: usd(day),
            });
        }
        let thread_spent = self.thread(thread).micro_usd;
        let cap_t = micro_from_usd(config.thread_cap_usd);
        if config.thread_cap_usd <= 0.0 || thread_spent + est > cap_t {
            return Err(CapRefusal {
                scope: "thread",
                cap_usd: config.thread_cap_usd,
                spent_usd: usd(thread_spent),
            });
        }
        Ok(())
    }

    /// The figures the page shows.
    pub fn view(&self, config: &AgentConfig, thread: Option<&str>) -> serde_json::Value {
        let totals = |t: Totals| serde_json::json!({"input_tokens": t.input, "output_tokens": t.output, "usd": usd(t.micro_usd), "calls": t.calls});
        serde_json::json!({
            "today": totals(self.today()),
            "daily_cap_usd": config.daily_cap_usd,
            "thread": thread.map(|t| totals(self.thread(t))),
            "thread_cap_usd": config.thread_cap_usd,
            "providers": config.providers.iter().map(|p| serde_json::json!({"id": p.id, "month": totals(self.month(&p.id)), "cap_usd": p.monthly_cap_usd})).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_right() {
        assert_eq!(day_key(0), "1970-01-01");
        assert_eq!(day_key(951_868_800_000), "2000-03-01");
        assert_eq!(day_key(1_793_000_000_000), "2026-10-26");
    }

    #[test]
    fn costs_use_micro_dollars_per_token_at_a_per_million_rate() {
        assert_eq!(
            cost_micro(
                Usage {
                    input: 1_000_000,
                    output: 0,
                    estimated: false
                },
                3.0,
                15.0
            ),
            3_000_000
        );
        assert_eq!(
            cost_micro(
                Usage {
                    input: 1000,
                    output: 100,
                    estimated: false
                },
                3.0,
                15.0
            ),
            4_500
        );
    }

    #[test]
    fn caps_trip_before_the_call_and_zero_means_no_call() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(&AppHome::at(dir.path().to_path_buf()));
        let mut config = AgentConfig::default();
        config.providers[0].monthly_cap_usd = 0.01;
        let p = config.providers[0].clone();
        assert!(ledger.check(&config, &p, "t1", 100).is_ok());
        ledger.charge(
            &p.id,
            "t1",
            Usage {
                input: 1000,
                output: 400,
                estimated: false,
            },
            9_000,
        );
        let r = ledger.check(&config, &p, "t1", 2000).unwrap_err();
        assert_eq!(r.scope, "provider");
        assert!(r.message().contains("monthly cap"));
        let mut zero = config.clone();
        zero.providers[0].monthly_cap_usd = 0.0;
        let r = ledger
            .check(&zero, &zero.providers[0].clone(), "t2", 1)
            .unwrap_err();
        assert!(r.message().contains("is zero"));
        // The ledger survives a restart.
        let again = Ledger::new(&AppHome::at(dir.path().to_path_buf()));
        assert_eq!(again.thread("t1").micro_usd, 9_000);
        assert_eq!(again.today().calls, 1);
    }

    #[test]
    fn the_thread_cap_and_the_daily_cap_apply_too() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(&AppHome::at(dir.path().to_path_buf()));
        let mut config = AgentConfig::default();
        let p = config.providers[1].clone();
        ledger.charge(&p.id, "t", Usage::default(), 1_900_000);
        assert_eq!(
            ledger.check(&config, &p, "t", 100_000).unwrap_err().scope,
            "thread"
        );
        config.thread_cap_usd = 100.0;
        config.daily_cap_usd = 1.0;
        assert_eq!(
            ledger.check(&config, &p, "t", 10).unwrap_err().scope,
            "daily"
        );
    }
}
