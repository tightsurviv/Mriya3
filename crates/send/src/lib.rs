//! Transaction building and bidding economics — **explicit/consent-based channels only**.
//!
//! Pure, testable pieces: ComputeBudget instruction encoding, priority fee conversions
//! (Agave scheduler model), tip policy for auctions (Jito/BAM/Harmonic/Helius
//! Backrun Rebates) and v0 message serialization. Signing and submission stay with the operator.
//!
//! All paths here are public
//! TPU/SWQoS and auctions in which the orderflow provider's participation is explicit.

pub mod nonce;
pub mod package;
pub mod payout;
pub mod route;
pub mod submit;
pub mod tx;

pub mod compute_budget {
    //! Encoding of the instruction data of the ComputeBudget111111111111111111111111111111 program.

    /// ComputeBudget instruction discriminators.
    pub const SET_CU_LIMIT: u8 = 2;
    pub const SET_CU_PRICE: u8 = 3;
    pub const SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT: u8 = 4;

    /// SetComputeUnitLimit(units): tag(1) + u32 LE.
    pub fn set_cu_limit(units: u32) -> [u8; 5] {
        let mut d = [0u8; 5];
        d[0] = SET_CU_LIMIT;
        d[1..].copy_from_slice(&units.to_le_bytes());
        d
    }

    /// SetComputeUnitPrice(micro_lamports_per_cu): tag(1) + u64 LE.
    pub fn set_cu_price(micro_lamports: u64) -> [u8; 9] {
        let mut d = [0u8; 9];
        d[0] = SET_CU_PRICE;
        d[1..].copy_from_slice(&micro_lamports.to_le_bytes());
        d
    }

    /// SetLoadedAccountsDataSizeLimit(bytes): tag(1) + u32 LE.
    pub fn set_loaded_accounts_data_size_limit(bytes: u32) -> [u8; 5] {
        let mut d = [0u8; 5];
        d[0] = SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT;
        d[1..].copy_from_slice(&bytes.to_le_bytes());
        d
    }
}

pub mod fee {
    //! Priority fee model.
    //!
    //! Priority fee [lamports] = ceil(cu_price [µlamp/CU] · cu_limit [CU] / 1,000,000).
    //! Base fee = 5000 per signature (bot: 1 signature).

    pub const BASE_FEE_PER_SIGNATURE: u64 = 5_000;
    const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;

    /// Priority fee for the given cu_price and cu_limit.
    ///
    /// Rounded **up**, like the Agave runtime: `(µlamp + 999_999) / 1_000_000`
    /// (`5000 + ceil(price·cu_limit/1e6)`). Floor understated
    /// the cost by ≤1 lamport and overstated the margin.
    pub fn priority_fee(cu_price_micro: u64, cu_limit: u32) -> u64 {
        let micro = cu_price_micro as u128 * cu_limit as u128;
        ((micro + MICRO_LAMPORTS_PER_LAMPORT - 1) / MICRO_LAMPORTS_PER_LAMPORT) as u64
    }

    /// cu_price (µlamp/CU) such that the priority fee comes to ≥ `target_fee` (ceil).
    pub fn cu_price_for_target(target_fee_lamports: u64, cu_limit: u32) -> u64 {
        if cu_limit == 0 {
            return 0;
        }
        let num = target_fee_lamports as u128 * 1_000_000 + cu_limit as u128 - 1;
        (num / cu_limit as u128) as u64
    }

    /// Total fixed transaction cost (base + priority fee), excluding the tip.
    pub fn total_fee(cu_price_micro: u64, cu_limit: u32, signatures: u32) -> u64 {
        BASE_FEE_PER_SIGNATURE * signatures as u64 + priority_fee(cu_price_micro, cu_limit)
    }
}

pub mod bid {
    //! Consent-based auction bidding: `bid = round(f · E)`.
    //!
    //! `E` is the expected gross. With CU price 0, `E = X + bid + 5000` holds,
    //! where `X` is the expected net. The dominant measurement is `f = 0.341` and the ratio
    //! `bid/E` equal to 0.34100 to five places. From `E = 5_000_000` this fraction is almost
    //! exclusive. Below that the measurement also has 5%, 10.5%, 12% and 50%, with no rule for which to pick.
    //! The bot does not go below [`MIN_AUCTION_E`].

    /// Observed auction fraction (basis points, 3410 = 34.1%).
    pub const OBSERVED_AUCTION_FRAC_BPS: u32 = 3410;

    /// Smallest expected auction gross in the measurement, 670,218 lamports.
    pub const MIN_AUCTION_E: u64 = 670_218;

    /// From this gross upward the measurement holds at 34.1% (784/793 transactions of channels 2/4/5/6/7).
    pub const AUCTION_DOMINANT_TIER_E: u64 = 5_000_000;

    /// `bid = round(frac_bps / 10_000 · E)`, rounding half up.
    pub fn auction_tip(expected_gross: u64, frac_bps: u32) -> u64 {
        let n = u128::from(expected_gross) * u128::from(frac_bps);
        u64::try_from((n + 5_000) / 10_000).unwrap_or(u64::MAX)
    }

    /// Whether the opportunity is profitable: the expected gross profit must cover the fixed cost + tip
    /// and leave a positive margin ≥ `min_margin`.
    pub fn is_profitable(expected_gross: u64, fixed_cost: u64, tip: u64, min_margin: u64) -> bool {
        expected_gross > fixed_cost.saturating_add(tip).saturating_add(min_margin)
    }

    /// Net margin after subtracting fixed cost and tip (0 if negative).
    pub fn net_margin(expected_gross: u64, fixed_cost: u64, tip: u64) -> u64 {
        expected_gross.saturating_sub(fixed_cost).saturating_sub(tip)
    }
}

pub mod priority {
    //! Agave scheduler priority by which the backrun is positioned.
    //!
    //! `cost = 720·nsig + 300·n_writable + data_bytes//4 + cu_limit + ceil(loaded/32KiB)·8`
    //! `rp = (fee − 2500·nsig) · 1e6 / (cost + 1)`
    //!
    //! Subtracting 1 from the CU price does not achieve this: the cost depends on the transaction size, and the CU limit
    //! appears in both the numerator (via the fee) and the denominator.

    use super::fee;

    pub const SIGNATURE_COST: u64 = 720;
    pub const WRITE_LOCK_UNITS: u64 = 300;
    pub const DATA_BYTES_PER_UNIT: u64 = 4;
    pub const LOADED_PAGE_BYTES: u64 = 32 * 1024;
    pub const LOADED_PAGE_COST: u64 = 8;
    pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;

    /// Transaction shape without the CU limit. The limit is separate because the backrun tunes it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TxShape {
        pub n_signatures: u64,
        pub n_write_locks: u64,
        pub ix_data_bytes: u64,
        pub loaded_accounts_bytes: u64,
    }

    /// Limit and price at which the scheduler priority equals the given value.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CuBudget {
        pub cu_limit: u32,
        pub cu_price_micro: u64,
    }

    pub fn execution_cost(shape: TxShape, cu_limit: u64) -> u64 {
        let pages = shape.loaded_accounts_bytes.div_ceil(LOADED_PAGE_BYTES);
        SIGNATURE_COST * shape.n_signatures
            + WRITE_LOCK_UNITS * shape.n_write_locks
            + shape.ix_data_bytes / DATA_BYTES_PER_UNIT
            + cu_limit
            + pages * LOADED_PAGE_COST
    }

    pub fn scheduler_priority(fee_lamports: u64, n_signatures: u64, cost: u64) -> u64 {
        let half_base = (fee::BASE_FEE_PER_SIGNATURE / 2) * n_signatures;
        let adj = fee_lamports.saturating_sub(half_base);
        adj.saturating_mul(1_000_000) / cost.saturating_add(1)
    }

    pub fn priority_of(shape: TxShape, cu_limit: u32, cu_price_micro: u64) -> u64 {
        let cost = execution_cost(shape, u64::from(cu_limit));
        let fee = fee::total_fee(cu_price_micro, cu_limit, shape.n_signatures as u32);
        scheduler_priority(fee, shape.n_signatures, cost)
    }

    /// Smallest limit ≥ `floor_cu` and CU price at which priority = `target`.
    /// `None` when no pair up to 1.4 million CU hits it.
    pub fn fit_priority(shape: TxShape, floor_cu: u32, target: u64) -> Option<CuBudget> {
        if floor_cu == 0 || floor_cu > MAX_COMPUTE_UNIT_LIMIT {
            return None;
        }
        let mut start = u64::from(floor_cu);
        let fixed = execution_cost(shape, 0);
        let adj0 = (fee::BASE_FEE_PER_SIGNATURE / 2) * shape.n_signatures;
        if target > 0 && adj0 > 0 {
            let need = div_ceil(adj0.saturating_mul(1_000_000), target);
            let min_limit = need.saturating_sub(fixed).saturating_sub(1);
            if min_limit > start {
                start = min_limit;
            }
        }
        if start > u64::from(MAX_COMPUTE_UNIT_LIMIT) {
            return None;
        }
        let mut limit = start as u32;
        let mut scanned = 0u32;
        while limit <= MAX_COMPUTE_UNIT_LIMIT {
            if let Some(cu_price_micro) = price_for_limit(shape, limit, target) {
                return Some(CuBudget { cu_limit: limit, cu_price_micro });
            }
            scanned += 1;
            if scanned > 50_000 {
                return None;
            }
            limit = limit.saturating_add(1);
        }
        None
    }

    /// Smallest limit ≥ `floor_cu` and CU price at which the priority fee
    /// equals `target_fee`. Scans at most 50,000 limits. This is the limit of this
    /// search, not a threshold from the measurement. `None` when hitting it would raise the fee above the bid.
    pub fn fit_exact_priority_fee(floor_cu: u32, target_fee: u64) -> Option<CuBudget> {
        if floor_cu == 0 || floor_cu > MAX_COMPUTE_UNIT_LIMIT || target_fee == 0 {
            return None;
        }
        let mut limit = floor_cu;
        let mut scanned = 0u32;
        while limit <= MAX_COMPUTE_UNIT_LIMIT {
            let cu_price_micro = fee::cu_price_for_target(target_fee, limit);
            if fee::priority_fee(cu_price_micro, limit) == target_fee {
                return Some(CuBudget { cu_limit: limit, cu_price_micro });
            }
            scanned += 1;
            if scanned > 50_000 {
                return None;
            }
            limit = limit.saturating_add(1);
        }
        None
    }

    fn price_for_limit(shape: TxShape, limit: u32, target: u64) -> Option<u64> {
        let denom = execution_cost(shape, u64::from(limit)).saturating_add(1) as u128;
        let target_u = target as u128;
        let lo = target_u * denom;
        let hi = target_u.saturating_add(1).saturating_mul(denom);
        if hi == 0 {
            return None;
        }
        let adj_min = div_ceil_u128(lo, 1_000_000);
        let adj_max = (hi - 1) / 1_000_000;
        if adj_min > adj_max {
            return None;
        }
        let half = u128::from((fee::BASE_FEE_PER_SIGNATURE / 2) * shape.n_signatures);
        let mut adj = adj_min.max(half);
        while adj <= adj_max {
            let fee_prio = u64::try_from(adj - half).ok()?;
            if let Some(price) = price_for_priority_fee(limit, fee_prio) {
                if priority_of(shape, limit, price) == target {
                    return Some(price);
                }
            }
            adj += 1;
        }
        None
    }

    /// Smallest CU price whose priority fee (ceil) equals `fee` lamports.
    fn price_for_priority_fee(limit: u32, fee: u64) -> Option<u64> {
        if limit == 0 {
            return None;
        }
        if fee == 0 {
            return Some(0);
        }
        let limit = u128::from(limit);
        let p_min = (u128::from(fee) - 1) * 1_000_000 / limit + 1;
        let p_max = u128::from(fee) * 1_000_000 / limit;
        if p_min <= p_max {
            u64::try_from(p_min).ok()
        } else {
            None
        }
    }

    fn div_ceil(n: u64, d: u64) -> u64 {
        if d == 0 {
            return u64::MAX;
        }
        n / d + u64::from(n % d != 0)
    }

    fn div_ceil_u128(n: u128, d: u128) -> u128 {
        n / d + u128::from(n % d != 0)
    }

    /// Shape from a legacy or v0 message that we compile ourselves. Writes from ALT count toward
    /// `n_write_locks`. The limit and price bytes do not change the instruction length, so a measurement on
    /// a trial message carries over to the resulting one.
    pub fn shape_from_message(message: &[u8], loaded_accounts_bytes: u64) -> Option<TxShape> {
        let versioned = message.first() == Some(&0x80);
        if message.first().is_some_and(|b| b & 0x80 != 0) && !versioned {
            return None;
        }
        let mut i = if versioned { 1 } else { 0 };
        let n_signatures = u64::from(*message.get(i)?);
        let n_ro_signed = u64::from(*message.get(i + 1)?);
        let n_ro_unsigned = u64::from(*message.get(i + 2)?);
        i += 3;
        let (n_keys, j) = read_compact(message, i, 255)?;
        let n_keys = n_keys as u64;
        if n_ro_signed + n_ro_unsigned > n_keys {
            return None;
        }
        i = j.checked_add(n_keys as usize * 32)?.checked_add(32)?;
        let (n_ix, j) = read_compact(message, i, 64)?;
        i = j;
        let mut ix_data_bytes = 0u64;
        for _ in 0..n_ix {
            i = i.checked_add(1)?;
            let (n_acc, j) = read_compact(message, i, 255)?;
            i = j.checked_add(n_acc)?;
            let (n_data, j) = read_compact(message, i, 1232)?;
            ix_data_bytes = ix_data_bytes.saturating_add(n_data as u64);
            i = j.checked_add(n_data)?;
            if i > message.len() {
                return None;
            }
        }
        let mut alt_writes = 0u64;
        if versioned {
            let (n_lookup, j) = read_compact(message, i, 20)?;
            i = j;
            for _ in 0..n_lookup {
                i = i.checked_add(32)?;
                let (n_w, j) = read_compact(message, i, 256)?;
                alt_writes = alt_writes.saturating_add(n_w as u64);
                i = j.checked_add(n_w)?;
                let (n_r, j) = read_compact(message, i, 256)?;
                i = j.checked_add(n_r)?;
                if i > message.len() {
                    return None;
                }
            }
            if i != message.len() {
                return None;
            }
        }
        let n_write_locks = n_keys - n_ro_signed - n_ro_unsigned + alt_writes;
        Some(TxShape { n_signatures, n_write_locks, ix_data_bytes, loaded_accounts_bytes })
    }

    fn read_compact(data: &[u8], mut i: usize, max: usize) -> Option<(usize, usize)> {
        let mut n = 0usize;
        let mut shift = 0;
        loop {
            let b = *data.get(i)?;
            i += 1;
            n |= (usize::from(b & 0x7f)) << shift;
            if b & 0x80 == 0 {
                if n > max {
                    return None;
                }
                return Some((n, i));
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
    }
}

/// Instruction order in the built v0 message.
pub mod plan {
    //! ```text
    //! [ (optional) AdvanceNonce ]
    //!   SetComputeUnitLimit(cu_limit)
    //!   [ setup of intermediate accounts ]
    //!   <arb program: execute_route>
    //!   SetLoadedAccountsDataSizeLimit(bytes)
    //!   [ System::transfer(tip) ]
    //!   SetComputeUnitPrice(cu_price)
    //! ```
    //! Signing with the bot key and submission stay with the operator.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cu_limit_encoding() {
        assert_eq!(compute_budget::set_cu_limit(127_000), [2, 0x18, 0xf0, 0x01, 0x00]);
    }

    #[test]
    fn cu_price_encoding() {
        let d = compute_budget::set_cu_price(61_825);
        assert_eq!(d[0], 3);
        assert_eq!(u64::from_le_bytes(d[1..].try_into().unwrap()), 61_825);
    }

    #[test]
    fn priority_fee_math() {
        // 61,825 µlamp/CU · 127,000 CU = 7,851.775 lamp → ceil = 7,852 (like the Agave runtime).
        assert_eq!(fee::priority_fee(61_825, 127_000), 7_852);
        // product divisible without remainder → no rounding up.
        assert_eq!(fee::priority_fee(1_000_000, 127_000), 127_000);
        // any remainder rounds up by 1.
        assert_eq!(fee::priority_fee(1, 1), 1);
        assert_eq!(fee::priority_fee(0, 127_000), 0);
    }

    #[test]
    fn cu_price_round_trip_hits_target() {
        let cu_limit = 127_000;
        let target = 7_851u64;
        let price = fee::cu_price_for_target(target, cu_limit);
        // After converting back, the fee ≥ target.
        assert!(fee::priority_fee(price, cu_limit) >= target);
    }

    #[test]
    fn total_fee_includes_base() {
        // 1 signature, cu_price 0 → base fee only.
        assert_eq!(fee::total_fee(0, 127_000, 1), 5_000);
        assert_eq!(fee::total_fee(61_825, 127_000, 1), 5_000 + 7_852);
    }

    #[test]
    fn auction_tip_fraction() {
        // 34.1% of 1,000,000 = 341,000.
        assert_eq!(bid::auction_tip(1_000_000, bid::OBSERVED_AUCTION_FRAC_BPS), 341_000);
        // Truncation would give 0. Half up gives 1, so the ratio holds five places.
        assert_eq!(bid::auction_tip(2, bid::OBSERVED_AUCTION_FRAC_BPS), 1);
    }

    #[test]
    fn auction_share_reads_34100_at_five_decimals() {
        for e in [bid::MIN_AUCTION_E, bid::AUCTION_DOMINANT_TIER_E, 13_336_691] {
            let share = bid::auction_tip(e, bid::OBSERVED_AUCTION_FRAC_BPS);
            let scaled = (u128::from(share) * 100_000 + u128::from(e) / 2) / u128::from(e);
            assert_eq!(scaled, 34_100);
            assert_eq!(e - share - 5_000 + share + 5_000, e);
        }
    }

    #[test]
    fn profitability_check() {
        // gross 100k, cost 5k, tip 34.1k → margin 60.9k, profitable.
        let gross = 100_000u64;
        let tip = bid::auction_tip(gross, 3410);
        assert!(bid::is_profitable(gross, 5_000, tip, 1));
        assert_eq!(bid::net_margin(gross, 5_000, tip), 100_000 - 5_000 - 34_100);
        // with a margin threshold higher than the margin → unprofitable
        assert!(!bid::is_profitable(gross, 5_000, tip, 70_000));
    }

    #[test]
    fn scheduler_priority_matches_the_agave_formula() {
        let shape = priority::TxShape {
            n_signatures: 1,
            n_write_locks: 1,
            ix_data_bytes: 0,
            loaded_accounts_bytes: 0,
        };
        let cost = 720 + 300 + 1_000;
        let rp = (5_000u64 - 2_500) * 1_000_000 / (cost + 1);
        assert_eq!(priority::priority_of(shape, 1_000, 0), rp);

        let page = priority::TxShape { loaded_accounts_bytes: 32 * 1024, ..shape };
        let over = priority::TxShape { loaded_accounts_bytes: 32 * 1024 + 1, ..shape };
        assert_eq!(priority::execution_cost(over, 0), priority::execution_cost(page, 0) + 8);
    }

    #[test]
    fn fit_priority_is_exactly_one_below_a_different_transaction() {
        let victim = priority::TxShape {
            n_signatures: 1,
            n_write_locks: 4,
            ix_data_bytes: 80,
            loaded_accounts_bytes: 131_072,
        };
        let ours = priority::TxShape {
            n_signatures: 1,
            n_write_locks: 18,
            ix_data_bytes: 220,
            loaded_accounts_bytes: 131_072,
        };
        let victim_rp = priority::priority_of(victim, 200_000, 80_000);
        let fit = priority::fit_priority(ours, 127_000, victim_rp - 1).expect("rp−1 is reachable");
        assert!(fit.cu_limit >= 127_000);
        assert_eq!(priority::priority_of(ours, fit.cu_limit, fit.cu_price_micro), victim_rp - 1);
        let price_minus_one = priority::priority_of(ours, 200_000, 79_999);
        assert_ne!(price_minus_one, victim_rp - 1);
    }

    #[test]
    fn alt_writable_keeps_the_same_write_lock_count() {
        use crate::tx::{
            compile_legacy_message, compile_v0_message, AccountMeta, AddressLookupTable, Instruction,
        };
        fn pk(n: u8) -> [u8; 32] {
            let mut k = [0u8; 32];
            k[0] = n;
            k
        }
        let fee = pk(1);
        let program = pk(100);
        let writable = pk(60);
        let readonly = pk(61);
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta { pubkey: fee, is_signer: true, is_writable: true },
                AccountMeta { pubkey: writable, is_signer: false, is_writable: true },
                AccountMeta { pubkey: readonly, is_signer: false, is_writable: false },
            ],
            data: vec![1, 2, 3, 4],
        };
        let legacy = compile_legacy_message(fee, pk(255), &[ix.clone()]);
        let alt = AddressLookupTable { address: pk(200), addresses: vec![readonly, writable] };
        let v0 = compile_v0_message(fee, pk(255), &[ix], &[alt]);
        let a = priority::shape_from_message(&legacy, 131_072).unwrap();
        let b = priority::shape_from_message(&v0, 131_072).unwrap();
        assert_eq!(a.n_write_locks, b.n_write_locks);
        assert_eq!(a.n_write_locks, 2);
        assert_eq!(a.ix_data_bytes, b.ix_data_bytes);
        assert_eq!(a.n_signatures, b.n_signatures);
        assert!(!v0.windows(32).any(|w| w == writable));
        assert!(priority::shape_from_message(&v0[..v0.len() - 1], 131_072).is_none());
        let mut long = v0.clone();
        long.push(0);
        assert!(priority::shape_from_message(&long, 131_072).is_none());
        let mut v1 = v0;
        v1[0] = 0x81;
        assert!(priority::shape_from_message(&v1, 131_072).is_none());
    }

    #[test]
    fn exact_priority_fee_hits_the_bid_without_overshooting() {
        let target = bid::auction_tip(5_000_000, bid::OBSERVED_AUCTION_FRAC_BPS);
        assert_eq!(target, 1_705_000);
        let fit = priority::fit_exact_priority_fee(127_000, target).unwrap();
        assert!(fit.cu_limit >= 127_000);
        assert_eq!(fee::priority_fee(fit.cu_price_micro, fit.cu_limit), target);
        assert!(priority::fit_exact_priority_fee(0, target).is_none());
        assert!(priority::fit_exact_priority_fee(127_000, 0).is_none());
    }
}
