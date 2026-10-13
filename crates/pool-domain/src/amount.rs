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

    /// A number of coins written as text, such as `10`, `0.5` or `12.34567891`,
    /// as an exact amount. Digits and at most one point only, with at most eight
    /// decimals: no signs, no exponents, no spaces inside. Nothing passes through
    /// a floating-point number.
    pub fn parse_coins(text: &str) -> Option<Amount> {
        let text = text.trim();
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !whole.chars().all(|c| c.is_ascii_digit())
            || !fraction.chars().all(|c| c.is_ascii_digit())
            || fraction.len() > 8
        {
            return None;
        }
        let whole_units = if whole.is_empty() { 0 } else { whole.parse::<u64>().ok()?.checked_mul(UNITS_PER_COIN)? };
        let padded = format!("{fraction:0<8}");
        let fraction_units = padded.parse::<u64>().ok()?;
        whole_units.checked_add(fraction_units).map(Amount)
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
    fn coins_are_read_from_text_exactly() {
        let units = |t: &str| Amount::parse_coins(t).map(Amount::units);
        assert_eq!(units("10"), Some(1_000_000_000));
        assert_eq!(units("0.5"), Some(50_000_000));
        assert_eq!(units(".5"), Some(50_000_000));
        assert_eq!(units("5."), Some(500_000_000));
        assert_eq!(units("12.34567891"), Some(1_234_567_891));
        assert_eq!(units("0.00000001"), Some(1), "a single smallest unit");
        assert_eq!(units(" 7 "), Some(700_000_000), "spaces around it are ignored");
        assert_eq!(units("0"), Some(0));
        assert_eq!(units("0.1"), Some(10_000_000), "no floating-point error: 0.1 is exactly a tenth");
        assert_eq!(units("1.1"), Some(110_000_000));
    }

    #[test]
    fn anything_that_is_not_a_plain_decimal_is_refused() {
        for bad in ["", ".", "-1", "+1", "1e3", "1,5", "ten", "1.123456789", "1 2", "0x10", "1..2", "٣", "NaN", "inf"] {
            assert_eq!(Amount::parse_coins(bad), None, "{bad:?}");
        }
        assert_eq!(Amount::parse_coins("184467440737.09551615").map(Amount::units), Some(u64::MAX), "the largest amount");
        assert_eq!(Amount::parse_coins("184467440737.09551616"), None, "one more overflows");
        assert_eq!(Amount::parse_coins("999999999999999999999"), None);
    }

    #[test]
    fn whole_coins_convert_and_overflow_is_none() {
        assert_eq!(Amount::from_coins(10), Some(Amount::new(1_000_000_000)));
        assert_eq!(Amount::from_coins(u64::MAX), None);
    }
}
