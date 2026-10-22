//! *Constant product* pools (x·y = k) with a fee: Raydium v4/CPMM, Pump AMM, Meteora DAMM.

/// Constant-product pool.
///
/// `reserve_in` / `reserve_out` are given *in the swap direction* by [`amount_out`].
/// `fee_bps` is the pool fee in basis points (e.g. 30 = 0.30%, 25 = 0.25%).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpPool {
    pub reserve_in: u64,
    pub reserve_out: u64,
    pub fee_bps: u32,
}

impl CpPool {
    pub fn new(reserve_in: u64, reserve_out: u64, fee_bps: u32) -> Self {
        debug_assert!(fee_bps < 10_000, "fee must be < 100%");
        Self { reserve_in, reserve_out, fee_bps }
    }

    /// Reversed pool (flips the swap direction).
    pub fn reversed(self) -> Self {
        Self { reserve_in: self.reserve_out, reserve_out: self.reserve_in, fee_bps: self.fee_bps }
    }
}

/// How much output token we get for `amount_in` (Uniswap v2 / Raydium formula).
///
/// `out = (in·(1−fee)·R_out) / (R_in + in·(1−fee))`, computed in u128 without losing precision.
/// Returns 0 for an empty pool or zero input.
pub fn amount_out(amount_in: u64, pool: CpPool) -> u64 {
    if amount_in == 0 || pool.reserve_in == 0 || pool.reserve_out == 0 {
        return 0;
    }
    let fee_num = (10_000 - pool.fee_bps) as u128; // fee numerator (scale 10_000)
    let ain = amount_in as u128 * fee_num;         // input after fee, ·10_000
    let numerator = ain * pool.reserve_out as u128;
    let denominator = pool.reserve_in as u128 * 10_000 + ain;
    (numerator / denominator) as u64
}

/// Cyclic route through two pools: token X → (pool1) → Y → (pool2) → X.
///
/// `pool1` is in the X→Y direction, `pool2` in the Y→X direction. Returns how much X comes back.
pub fn two_pool_out(amount_in_x: u64, pool1: CpPool, pool2: CpPool) -> u64 {
    let y = amount_out(amount_in_x, pool1);
    amount_out(y, pool2)
}

/// Net profit (in X units) from a two-pool route for a given input: `X_back − X_in`.
pub fn two_pool_profit(amount_in_x: u64, pool1: CpPool, pool2: CpPool) -> i128 {
    let back = two_pool_out(amount_in_x, pool1, pool2) as i128;
    back - amount_in_x as i128
}

/// Result of input-size optimization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opt {
    /// Optimal input size in X (0 = not worth trading).
    pub amount_in: u64,
    /// Net profit at this size (≤ 0 means no opportunity).
    pub profit: i128,
}

/// Finds the input size that maximizes profit on a two-pool route.
///
/// Profit(x) is unimodal on these curves (rises, then falls), so we use
/// ternary search, and close the final interval with a scan. `cap` limits
/// the input (e.g. to the capital held or a fraction of the reserve). If nothing is
/// positive, returns `amount_in = 0, profit = 0` (better not to trade).
pub fn optimal_two_pool(pool1: CpPool, pool2: CpPool, cap: u64) -> Opt {
    if cap == 0 {
        return Opt { amount_in: 0, profit: 0 };
    }
    let f = |x: u64| two_pool_profit(x, pool1, pool2);

    let (mut lo, mut hi) = (0u64, cap);
    while hi - lo > 2 {
        let third = (hi - lo) / 3;
        let m1 = lo + third;
        let m2 = hi - third;
        if f(m1) < f(m2) {
            lo = m1;
        } else {
            hi = m2;
        }
    }

    // Closing: best point in [lo, hi], with a "don't trade" option (x=0, profit 0).
    let mut best = Opt { amount_in: 0, profit: 0 };
    for x in lo..=hi {
        let p = f(x);
        if p > best.profit {
            best = Opt { amount_in: x, profit: p };
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_out_uniswap_v2_reference() {
        // Reserves 1000/1000, fee 0.30%, input 100 → 90 (reference value).
        let p = CpPool::new(1_000, 1_000, 30);
        assert_eq!(amount_out(100, p), 90);
    }

    #[test]
    fn amount_out_edge_cases() {
        let p = CpPool::new(1_000, 1_000, 30);
        assert_eq!(amount_out(0, p), 0);
        assert_eq!(amount_out(100, CpPool::new(0, 1_000, 30)), 0);
        assert_eq!(amount_out(100, CpPool::new(1_000, 0, 30)), 0);
    }

    #[test]
    fn amount_out_no_overflow_large_reserves() {
        // Close to the u64 limits — checks that computing in u128 does not overflow.
        let p = CpPool::new(u64::MAX / 2, u64::MAX / 2, 25);
        let out = amount_out(1_000_000_000, p);
        assert!(out > 0 && out < 1_000_000_000);
    }

    #[test]
    fn output_is_monotonic_with_diminishing_returns() {
        let p = CpPool::new(1_000_000, 1_000_000, 30);
        let a = amount_out(1_000, p);
        let b = amount_out(2_000, p);
        assert!(b > a, "more input → more output");
        assert!(b < 2 * a, "diminishing increments (slippage)");
    }

    #[test]
    fn no_arbitrage_between_equal_pools() {
        // Two identical pools at the same price — no opportunity.
        let p1 = CpPool::new(1_000_000, 1_000_000, 30); // X→Y
        let p2 = CpPool::new(1_000_000, 1_000_000, 30); // Y→X
        let opt = optimal_two_pool(p1, p2, 100_000);
        assert_eq!(opt.amount_in, 0);
        assert_eq!(opt.profit, 0);
    }

    #[test]
    fn finds_profit_on_price_gap() {
        // Pool 1 (X→Y): lots of Y per X (Y cheap). Pool 2 (Y→X): lots of X per Y (Y expensive).
        // We buy Y cheap on pool 1, sell it dear on pool 2.
        let p1 = CpPool::new(1_000_000, 1_200_000, 30); // X→Y, 1 X ≈ 1.2 Y
        let p2 = CpPool::new(1_000_000, 1_200_000, 30); // Y→X, 1 Y ≈ 1.2 X
        let opt = optimal_two_pool(p1, p2, 500_000);
        assert!(opt.profit > 0, "there should be a positive opportunity");
        assert!(opt.amount_in > 0 && opt.amount_in <= 500_000);
        // The optimum is a true maximum: neighboring inputs do not give more.
        if opt.amount_in > 1 {
            assert!(two_pool_profit(opt.amount_in - 1, p1, p2) <= opt.profit);
        }
        assert!(two_pool_profit(opt.amount_in + 1, p1, p2) <= opt.profit);
    }

    #[test]
    fn cap_limits_input() {
        let p1 = CpPool::new(1_000_000, 2_000_000, 30);
        let p2 = CpPool::new(1_000_000, 2_000_000, 30);
        let cap = 1_000;
        let opt = optimal_two_pool(p1, p2, cap);
        assert!(opt.amount_in <= cap);
    }
}
