use pool_chain::Chain;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Both chains this pool serves count eight decimal places: one coin is a
/// hundred million of the smallest unit.
pub const UNITS_PER_COIN: u64 = 100_000_000;

/// A sum of money in the chain's smallest unit, as a whole number. Money is
/// never a float anywhere in the pool (PLAN.md ground rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Amount(u64);

impl Amount {
    pub const ZERO: Amount = Amount(0);

    pub const fn new(units: u64) -> Self {
        Amount(units)
    }

    /// `coins` whole coins, or `None` if that does not fit in a `u64`.
    pub fn from_coins(coins: u64) -> Option<Self> {
        coins.checked_mul(UNITS_PER_COIN).map(Amount)
    }

    pub const fn units(self) -> u64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn checked_add(self, other: Amount) -> Option<Amount> {
        self.0.checked_add(other.0).map(Amount)
    }

    pub fn checked_sub(self, other: Amount) -> Option<Amount> {
        self.0.checked_sub(other.0).map(Amount)
    }

    pub fn saturating_sub(self, other: Amount) -> Amount {
        Amount(self.0.saturating_sub(other.0))
    }

    /// This amount as people read it: coins with two decimals and the chain's
    /// ticker, such as `12.34 MAGLD`. The third decimal is cut off, never
    /// rounded up, so nobody is ever shown more than they have.
    pub fn display<C: Chain>(self) -> AmountDisplay {
        AmountDisplay {
            whole: self.0 / UNITS_PER_COIN,
            hundredths: (self.0 % UNITS_PER_COIN) / (UNITS_PER_COIN / 100),
            ticker: C::ticker(),
        }
    }
}

/// What `Amount::display` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmountDisplay {
    whole: u64,
    hundredths: u64,
    ticker: &'static str,
}

impl fmt::Display for AmountDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02} {}", self.whole, self.hundredths, self.ticker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_chain::FakeChain;

    fn shown(units: u64) -> String {
        Amount::new(units).display::<FakeChain>().to_string()
    }

    #[test]
    fn amounts_read_as_coins_with_two_decimals_and_the_ticker() {
        assert_eq!(shown(0), "0.00 TST");
        assert_eq!(shown(1), "0.00 TST");
        assert_eq!(shown(1_000_000), "0.01 TST");
        assert_eq!(shown(1_234_000_000), "12.34 TST");
        assert_eq!(shown(100_000_000), "1.00 TST");
        assert_eq!(shown(5_000_000_000), "50.00 TST");
    }

    #[test]
    fn the_third_decimal_is_cut_not_rounded() {
        assert_eq!(shown(1_234_999_999), "12.34 TST");
        assert_eq!(shown(99_999_999), "0.99 TST");
    }

    #[test]
    fn the_largest_amount_still_displays() {
        assert_eq!(shown(u64::MAX), "184467440737.09 TST");
    }

    #[test]
    fn arithmetic_is_checked() {
        let a = Amount::new(10);
        assert_eq!(a.checked_add(Amount::new(5)), Some(Amount::new(15)));
        assert_eq!(a.checked_sub(Amount::new(11)), None);
        assert_eq!(a.saturating_sub(Amount::new(11)), Amount::ZERO);
        assert_eq!(Amount::new(u64::MAX).checked_add(Amount::new(1)), None);
        assert!(Amount::ZERO.is_zero());
    }

    #[test]
    fn whole_coins_convert_and_overflow_is_none() {
        assert_eq!(Amount::from_coins(10), Some(Amount::new(1_000_000_000)));
        assert_eq!(Amount::from_coins(u64::MAX), None);
    }
}
