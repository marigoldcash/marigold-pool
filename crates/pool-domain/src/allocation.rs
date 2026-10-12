//! Splitting one block reward among the miners whose shares were in the window.
//!
//! Everything here is whole numbers (PLAN.md ground rule 6). The result adds up
//! to the reward exactly, every time, with no remainder left behind: the pool's
//! fee is cut off first, and the rest is divided by the largest-remainder
//! method, which hands out the last few units to the miners whose exact share
//! had the biggest fractional part.

use crate::{Amount, MinerAddress, ShareWeight};
use std::collections::BTreeMap;

/// A fee of 100% in basis points. A larger setting is treated as this.
pub const MAX_FEE_BP: u16 = 10_000;

/// How one reward was split. `fee + the sum of per_miner == the reward`, always.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// What the pool keeps.
    pub fee: Amount,
    /// What each miner gets, in address order. A miner who would get nothing is
    /// left out.
    pub per_miner: Vec<(MinerAddress, Amount)>,
}

/// Split `reward` among `shares`.
///
/// * The fee is `reward × fee_bp / 10 000`, rounded down. `fee_bp` above 10 000
///   counts as 10 000.
/// * What is left goes to the miners in proportion to their summed weight. The
///   same address may appear more than once and is summed; a weight of nothing
///   counts for nothing.
/// * Units that do not divide evenly go one each to the miners with the largest
///   fractional shares; when those are equal, the lower address goes first. The
///   order the shares arrive in never changes the answer.
/// * With no weight at all, the whole reward is the fee.
pub fn allocate(reward: Amount, fee_bp: u16, shares: &[(MinerAddress, ShareWeight)]) -> Allocation {
    let total = u128::from(reward.units());

    let mut weights: BTreeMap<&MinerAddress, u128> = BTreeMap::new();
    for (address, weight) in shares {
        if weight.units() > 0 {
            *weights.entry(address).or_insert(0) += u128::from(weight.units());
        }
    }
    let total_weight: u128 = weights.values().sum();
    if total_weight == 0 {
        return Allocation { fee: reward, per_miner: Vec::new() };
    }

    let fee = total * u128::from(fee_bp.min(MAX_FEE_BP)) / u128::from(MAX_FEE_BP);
    let net = total - fee;

    // Each miner's exact share is net × weight / total_weight; keep the whole
    // part and the remainder of that division.
    let mut parts: Vec<(&MinerAddress, u128, u128)> = Vec::with_capacity(weights.len());
    let mut handed_out: u128 = 0;
    for (address, weight) in &weights {
        let exact = net * weight;
        let whole = exact / total_weight;
        handed_out += whole;
        parts.push((*address, whole, exact % total_weight));
    }

    // The units left over are fewer than the number of miners.
    let mut leftover = net - handed_out;
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|&a, &b| parts[b].2.cmp(&parts[a].2).then_with(|| parts[a].0.cmp(parts[b].0)));
    for &index in &order {
        if leftover == 0 {
            break;
        }
        parts[index].1 += 1;
        leftover -= 1;
    }

    let per_miner = parts
        .into_iter()
        .filter(|(_, amount, _)| *amount > 0)
        .map(|(address, amount, _)| (address.clone(), Amount::new(amount as u64)))
        .collect();
    Allocation { fee: Amount::new(fee as u64), per_miner }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(name: &str) -> MinerAddress {
        MinerAddress::from_trusted(format!("fake:{name}"))
    }

    fn shares(list: &[(&str, u64)]) -> Vec<(MinerAddress, ShareWeight)> {
        list.iter().map(|(n, w)| (who(n), ShareWeight::new(*w))).collect()
    }

    fn paid(allocation: &Allocation) -> Vec<(String, u64)> {
        allocation.per_miner.iter().map(|(a, n)| (a.as_str().trim_start_matches("fake:").to_owned(), n.units())).collect()
    }

    fn total(allocation: &Allocation) -> u64 {
        allocation.fee.units() + allocation.per_miner.iter().map(|(_, n)| n.units()).sum::<u64>()
    }

    #[test]
    fn an_even_split_after_the_fee() {
        let split = allocate(Amount::new(1_000), 100, &shares(&[("a", 1), ("b", 1), ("c", 1)]));
        assert_eq!(split.fee.units(), 10);
        assert_eq!(paid(&split), [("a".into(), 330), ("b".into(), 330), ("c".into(), 330)]);
    }

    #[test]
    fn a_split_in_proportion_to_weight() {
        let split = allocate(Amount::new(10), 0, &shares(&[("a", 5), ("b", 3), ("c", 2)]));
        assert_eq!(split.fee.units(), 0);
        assert_eq!(paid(&split), [("a".into(), 5), ("b".into(), 3), ("c".into(), 2)]);
    }

    #[test]
    fn a_leftover_unit_goes_to_the_lower_address_when_the_fractions_tie() {
        let split = allocate(Amount::new(100), 0, &shares(&[("c", 1), ("a", 1), ("b", 1)]));
        assert_eq!(paid(&split), [("a".into(), 34), ("b".into(), 33), ("c".into(), 33)]);
    }

    #[test]
    fn leftover_units_go_to_the_biggest_fractions_first() {
        // Exact shares of 10: a 3.3333, b 3.3333, c 3.3333 would tie; make them differ.
        // weights 5,3,2 of 3 units: exact 1.5, 0.9, 0.6 -> floors 1,0,0, one left for the largest fraction (b, .9).
        let split = allocate(Amount::new(3), 0, &shares(&[("a", 5), ("b", 3), ("c", 2)]));
        assert_eq!(paid(&split), [("a".into(), 1), ("b".into(), 1), ("c".into(), 1)]);
        // weights 7,2,1 of 5 units: exact 3.5, 1.0, 0.5 -> floors 3,1,0, one left; fractions .5,0,.5 tie a vs c -> lower address a.
        let split = allocate(Amount::new(5), 0, &shares(&[("a", 7), ("b", 2), ("c", 1)]));
        assert_eq!(paid(&split), [("a".into(), 4), ("b".into(), 1)]);
    }

    #[test]
    fn the_fee_is_rounded_down() {
        let split = allocate(Amount::new(199), 100, &shares(&[("a", 1)]));
        assert_eq!(split.fee.units(), 1, "1.99 becomes 1");
        assert_eq!(paid(&split), [("a".into(), 198)]);
    }

    #[test]
    fn no_shares_means_the_pool_keeps_the_lot() {
        let split = allocate(Amount::new(777), 100, &[]);
        assert_eq!((split.fee.units(), split.per_miner.len()), (777, 0));
    }

    #[test]
    fn shares_that_weigh_nothing_are_the_same_as_no_shares() {
        let split = allocate(Amount::new(777), 100, &shares(&[("a", 0), ("b", 0)]));
        assert_eq!((split.fee.units(), split.per_miner.len()), (777, 0));
    }

    #[test]
    fn the_same_address_twice_is_summed_and_a_zero_weight_is_ignored() {
        let split = allocate(Amount::new(400), 0, &shares(&[("a", 1), ("b", 1), ("a", 2), ("c", 0)]));
        assert_eq!(paid(&split), [("a".into(), 300), ("b".into(), 100)]);
    }

    #[test]
    fn a_fee_of_everything_leaves_nothing_for_the_miners() {
        let split = allocate(Amount::new(500), 10_000, &shares(&[("a", 1)]));
        assert_eq!((split.fee.units(), split.per_miner.len()), (500, 0));
        let over = allocate(Amount::new(500), u16::MAX, &shares(&[("a", 1)]));
        assert_eq!((over.fee.units(), over.per_miner.len()), (500, 0), "more than 100% counts as 100%");
    }

    #[test]
    fn a_reward_of_nothing_splits_into_nothing() {
        let split = allocate(Amount::ZERO, 100, &shares(&[("a", 1)]));
        assert_eq!((split.fee.units(), split.per_miner.len()), (0, 0));
    }

    #[test]
    fn a_miner_whose_share_rounds_to_nothing_is_left_out_but_the_sum_still_holds() {
        let split = allocate(Amount::new(5), 0, &shares(&[("a", 1_000_000), ("b", 1)]));
        assert_eq!(paid(&split), [("a".into(), 5)]);
        assert_eq!(total(&split), 5);
    }

    #[test]
    fn the_order_the_shares_arrive_in_changes_nothing() {
        let forward = shares(&[("a", 7), ("b", 11), ("c", 13), ("d", 17), ("e", 19)]);
        let mut backward = forward.clone();
        backward.reverse();
        let mut shuffled = forward.clone();
        shuffled.swap(0, 3);
        shuffled.swap(1, 4);
        let one = allocate(Amount::new(1_000_003), 137, &forward);
        assert_eq!(one, allocate(Amount::new(1_000_003), 137, &backward));
        assert_eq!(one, allocate(Amount::new(1_000_003), 137, &shuffled));
    }

    #[test]
    fn the_largest_numbers_do_not_overflow() {
        let split = allocate(Amount::new(u64::MAX), 100, &shares(&[("a", u64::MAX), ("b", u64::MAX - 1), ("c", 1)]));
        assert_eq!(total(&split), u64::MAX);
        let split = allocate(Amount::new(u64::MAX), 0, &shares(&[("a", u64::MAX)]));
        assert_eq!(paid(&split), [("a".into(), u64::MAX)]);
    }

    /// A small deterministic random source, so the property test needs no crate
    /// and fails the same way every time.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn a_thousand_random_splits_always_add_up_and_stay_fair() {
        let mut rng = Rng(0x00C0_FFEE);
        for case in 0..1_000 {
            let magnitude = [1u64, 10, 1_000, 1_000_000, 1_000_000_000_000, u64::MAX][rng.below(6) as usize];
            let reward = rng.below(magnitude.saturating_add(1).max(1)).max(if rng.below(10) == 0 { 0 } else { 1 });
            let fee_bp = rng.below(10_001) as u16;
            let miners = rng.below(31);
            let weight_cap = [1u64, 7, 1_000, 1_000_000, u64::MAX >> 8][rng.below(5) as usize];
            let list: Vec<(MinerAddress, ShareWeight)> =
                (0..miners).map(|_| (who(&format!("m{:02}", rng.below(20))), ShareWeight::new(rng.below(weight_cap + 1)))).collect();

            let split = allocate(Amount::new(reward), fee_bp, &list);

            // 1. Exactly the reward, never a unit more or less.
            assert_eq!(total(&split), reward, "case {case}: reward {reward} fee_bp {fee_bp}");

            // Per-address summed weights, for the fairness checks.
            let mut summed: BTreeMap<String, u128> = BTreeMap::new();
            for (a, w) in &list {
                *summed.entry(a.to_string()).or_default() += u128::from(w.units());
            }
            let total_weight: u128 = summed.values().sum();
            if total_weight == 0 {
                assert_eq!((split.fee.units(), split.per_miner.len()), (reward, 0), "case {case}: no weight");
                continue;
            }

            // 2. The fee is the rounded-down percentage.
            assert_eq!(u128::from(split.fee.units()), u128::from(reward) * u128::from(fee_bp) / 10_000, "case {case}: fee");

            // 3. Everyone gets their exact share rounded down or up, never further.
            let net = u128::from(reward) - u128::from(split.fee.units());
            let got: BTreeMap<String, u128> = split.per_miner.iter().map(|(a, n)| (a.to_string(), u128::from(n.units()))).collect();
            for (address, weight) in &summed {
                let floor = net * weight / total_weight;
                let amount = got.get(address).copied().unwrap_or(0);
                assert!(amount == floor || amount == floor + 1, "case {case}: {address} got {amount}, exact share is {floor}.x");
            }

            // 4. More weight never means less money.
            for (a, wa) in &summed {
                for (b, wb) in &summed {
                    if wa > wb {
                        assert!(
                            got.get(a).copied().unwrap_or(0) >= got.get(b).copied().unwrap_or(0),
                            "case {case}: {a} outweighs {b} yet gets less"
                        );
                    }
                }
            }

            // 5. Addresses come out in order, with no zero entries.
            assert!(split.per_miner.windows(2).all(|w| w[0].0 < w[1].0), "case {case}: order");
            assert!(split.per_miner.iter().all(|(_, n)| !n.is_zero()), "case {case}: zero entry");
        }
    }
}
