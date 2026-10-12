use serde::Serialize;

/// How much one share counts when a reward is split: the share's difficulty
/// times a thousand, as a whole number, so no money arithmetic ever touches a
/// float (PLAN.md ground rule 6). Difficulty 1 is weight 1000, and difficulty
/// 0.0005 rounds to weight 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(transparent)]
pub struct ShareWeight(u64);

impl ShareWeight {
    pub const ZERO: ShareWeight = ShareWeight(0);

    pub const fn new(units: u64) -> Self {
        ShareWeight(units)
    }

    /// From the stratum difficulty a share was checked against. A difficulty
    /// that is not a positive finite number counts for nothing, and an absurdly
    /// large one saturates.
    pub fn from_difficulty(difficulty: f64) -> Self {
        if !difficulty.is_finite() || difficulty <= 0.0 {
            return ShareWeight::ZERO;
        }
        // Casting a float to an integer saturates at the integer's bounds.
        ShareWeight((difficulty * 1000.0).round() as u64)
    }

    pub const fn units(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_times_a_thousand() {
        assert_eq!(ShareWeight::from_difficulty(1.0).units(), 1000);
        assert_eq!(ShareWeight::from_difficulty(4.0).units(), 4000);
        assert_eq!(ShareWeight::from_difficulty(2048.0).units(), 2_048_000);
    }

    #[test]
    fn it_rounds_to_the_nearest_whole_weight() {
        assert_eq!(ShareWeight::from_difficulty(0.0005).units(), 1);
        assert_eq!(ShareWeight::from_difficulty(0.0004).units(), 0);
        assert_eq!(ShareWeight::from_difficulty(1.2344).units(), 1234);
        assert_eq!(ShareWeight::from_difficulty(1.2346).units(), 1235);
    }

    #[test]
    fn nonsense_counts_for_nothing() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
            assert_eq!(ShareWeight::from_difficulty(bad), ShareWeight::ZERO, "{bad}");
        }
    }

    #[test]
    fn an_enormous_difficulty_saturates() {
        assert_eq!(ShareWeight::from_difficulty(1e300).units(), u64::MAX);
    }
}
