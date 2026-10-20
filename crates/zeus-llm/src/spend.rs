//! Spend accounting with hard caps. All amounts are integer micro-USD (1e-6 USD), so no floating point.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

/// Hard caps. Zero means "no spending allowed", not "unlimited".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpendCaps {
    /// Maximum per UTC day.
    pub daily_micro_usd: u64,
    /// Maximum per UTC month.
    pub monthly_micro_usd: u64,
}

impl Default for SpendCaps {
    /// One US dollar a day and ten a month: low enough that a runaway loop is cheap.
    fn default() -> Self {
        Self {
            daily_micro_usd: 1_000_000,
            monthly_micro_usd: 10_000_000,
        }
    }
}

/// Running totals for the current day and month. The caller supplies the period keys so the ledger has no clock
/// and tests are deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLedger {
    caps: SpendCaps,
    day_key: u32,
    month_key: u32,
    day_spent: u64,
    month_spent: u64,
}

impl SpendLedger {
    /// Start a ledger at the given period keys. `day_key` is days since the Unix epoch (UTC) and `month_key` is
    /// `year * 12 + (month - 1)`.
    pub fn new(caps: SpendCaps, day_key: u32, month_key: u32) -> Self {
        Self {
            caps,
            day_key,
            month_key,
            day_spent: 0,
            month_spent: 0,
        }
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn set_caps(&mut self, caps: SpendCaps) {
        self.caps = caps;
    }

    /// Current caps.
    pub fn caps(&self) -> SpendCaps {
        self.caps
    }

    /// Spent in the current day.
    pub fn day_spent(&self) -> u64 {
        self.day_spent
    }

    /// Spent in the current month.
    pub fn month_spent(&self) -> u64 {
        self.month_spent
    }

    /// Roll the totals over when a period changes. Keys must not go backwards; a backwards key is ignored so a
    /// clock step cannot reset spending.
    pub fn advance(&mut self, day_key: u32, month_key: u32) {
        if month_key > self.month_key {
            self.month_key = month_key;
            self.month_spent = 0;
        }
        if day_key > self.day_key {
            self.day_key = day_key;
            self.day_spent = 0;
        }
    }

    /// Whether `cost` more micro-USD would stay within both caps. Does not change the ledger.
    pub fn check(&self, cost_micro_usd: u64) -> Result<(), LlmError> {
        let day = self.day_spent.checked_add(cost_micro_usd);
        if day.is_none_or(|day| day > self.caps.daily_micro_usd) {
            return Err(LlmError::BudgetExceeded {
                period: "daily",
                spent_micro_usd: self.day_spent,
                cap_micro_usd: self.caps.daily_micro_usd,
            });
        }
        let month = self.month_spent.checked_add(cost_micro_usd);
        if month.is_none_or(|month| month > self.caps.monthly_micro_usd) {
            return Err(LlmError::BudgetExceeded {
                period: "monthly",
                spent_micro_usd: self.month_spent,
                cap_micro_usd: self.caps.monthly_micro_usd,
            });
        }
        Ok(())
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn refund(&mut self, day: u32, month: u32, amount: u64) {
        if day == self.day_key {
            self.day_spent = self.day_spent.saturating_sub(amount);
        }
        if month == self.month_key {
            self.month_spent = self.month_spent.saturating_sub(amount);
        }
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn record_overrun(&mut self, day: u32, month: u32, amount: u64) {
        // Observed usage is recorded even when it exceeds a cap; that cap prevents subsequent calls.
        if day == self.day_key {
            self.day_spent = self.day_spent.saturating_add(amount);
        }
        if month == self.month_key {
            self.month_spent = self.month_spent.saturating_add(amount);
        }
    }

    /// Record a cost, refusing it when a cap would be exceeded.
    pub fn charge(&mut self, cost_micro_usd: u64) -> Result<(), LlmError> {
        self.check(cost_micro_usd)?;
        self.day_spent += cost_micro_usd;
        self.month_spent += cost_micro_usd;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn charge_within_caps_accumulates() {
        let mut ledger = SpendLedger::new(
            SpendCaps {
                daily_micro_usd: 100,
                monthly_micro_usd: 250,
            },
            10,
            1,
        );
        ledger.charge(60).unwrap();
        ledger.charge(40).unwrap();
        assert_eq!(ledger.day_spent(), 100);
        assert!(matches!(
            ledger.charge(1),
            Err(LlmError::BudgetExceeded {
                period: "daily",
                ..
            })
        ));
    }

    #[test]
    fn new_day_resets_daily_but_not_monthly() {
        let mut ledger = SpendLedger::new(
            SpendCaps {
                daily_micro_usd: 100,
                monthly_micro_usd: 150,
            },
            10,
            1,
        );
        ledger.charge(100).unwrap();
        ledger.advance(11, 1);
        ledger.charge(50).unwrap();
        assert!(matches!(
            ledger.charge(1),
            Err(LlmError::BudgetExceeded {
                period: "monthly",
                ..
            })
        ));
        ledger.advance(40, 2);
        ledger.charge(100).unwrap();
    }

    #[test]
    fn backwards_clock_cannot_reset_spending() {
        let mut ledger = SpendLedger::new(
            SpendCaps {
                daily_micro_usd: 100,
                monthly_micro_usd: 100,
            },
            10,
            5,
        );
        ledger.charge(100).unwrap();
        ledger.advance(3, 2);
        assert!(ledger.charge(1).is_err());
    }

    #[test]
    fn zero_cap_allows_nothing() {
        let mut ledger = SpendLedger::new(
            SpendCaps {
                daily_micro_usd: 0,
                monthly_micro_usd: 0,
            },
            0,
            0,
        );
        assert!(ledger.charge(1).is_err());
        ledger.charge(0).unwrap();
    }

    #[test]
    fn maximal_cap_cannot_overflow_the_ledger() {
        let mut ledger = SpendLedger::new(
            SpendCaps {
                daily_micro_usd: u64::MAX,
                monthly_micro_usd: u64::MAX,
            },
            1,
            1,
        );
        ledger.charge(u64::MAX).unwrap();
        assert!(ledger.charge(1).is_err());
        assert_eq!(ledger.day_spent(), u64::MAX);
        ledger.advance(2, 1);
        assert!(ledger.charge(1).is_err());
        assert_eq!(ledger.day_spent(), 0);
        assert_eq!(ledger.month_spent(), u64::MAX);
    }

    proptest! {
        #[test]
        fn never_exceeds_caps(costs in proptest::collection::vec(0u64..500, 0..64), day_cap in 0u64..2000, month_cap in 0u64..4000) {
            let mut ledger = SpendLedger::new(SpendCaps { daily_micro_usd: day_cap, monthly_micro_usd: month_cap }, 1, 1);
            for cost in costs {
                let _ = ledger.charge(cost);
                prop_assert!(ledger.day_spent() <= day_cap);
                prop_assert!(ledger.month_spent() <= month_cap);
            }
        }
    }
}
