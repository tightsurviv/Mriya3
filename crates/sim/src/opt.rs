//! General input-size selection for a profit function unimodal in `x`.
//!
//! Profit from an arbitrage route (input → … → base) rises, then falls with the input
//! size (slippage). Such functions are unimodal, so we search for the maximum by ternary search and
//! close the final interval with a scan. Works for ANY exchange — just pass a
//! closure that computes profit for a given input.

use crate::Opt;

/// Maximizes `f(x)` for `x` in `[0, cap]`, assuming unimodality.
/// Returns the best input and profit; `(0, 0)` if nothing is positive (do not trade).
pub fn argmax_unimodal(cap: u64, f: impl Fn(u64) -> i128) -> Opt {
    if cap == 0 {
        return Opt { amount_in: 0, profit: 0 };
    }
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
    fn maximizes_concave_quadratic() {
        // −(x−100)^2 + 500 has its maximum at x=100 (profit 500).
        let f = |x: u64| 500i128 - (x as i128 - 100).pow(2);
        let o = argmax_unimodal(1000, f);
        assert_eq!(o.amount_in, 100);
        assert_eq!(o.profit, 500);
    }

    #[test]
    fn returns_zero_when_never_positive() {
        let f = |x: u64| -(x as i128) - 1;
        let o = argmax_unimodal(1000, f);
        assert_eq!(o.amount_in, 0);
        assert_eq!(o.profit, 0);
    }
}
