//! DLMM simulator (Meteora) — liquidity concentrated in *bins* of constant price.
//!
//! Unlike constant-product, within a single bin a constant-sum rule applies:
//! exchange at the bin's fixed price until its reserve on the given side is exhausted, then
//! move on to the next bin. The price of bin `i` is `(1 + bin_step)^i`; here we give it
//! directly as `price_x64` (amount of Y per 1 X in Q64.64 format), since we will compute it when
//! decoding the pool state. Integer math, verified against arbitrary-precision
//! arithmetic.
//!
//! Scope of the first version: base fee (constant). Meteora's variable fee (volatility
//! accumulator) will come once we hook up decoding of the full pool state.

use crate::math::{mul_div_ceil, mul_div_floor};

const Q64: u128 = 1u128 << 64;

/// A single DLMM bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bin {
    /// Bin price: amount of Y per 1 X, in Q64.64 format.
    pub price_x64: u128,
    pub reserve_x: u64,
    pub reserve_y: u64,
}

fn after_fee(amount: u64, fee_bps: u32) -> u128 {
    amount as u128 * (10_000 - fee_bps) as u128 / 10_000
}

fn sat_u64(v: u128) -> u64 {
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

/// Buy X with Y (price rises): bins given ascending from the active one upward.
/// Returns the amount of X out.
pub fn swap_y_for_x(bins_ascending: &[Bin], amount_y: u64, fee_bps: u32) -> u64 {
    let mut rem = after_fee(amount_y, fee_bps);
    let mut x_out: u128 = 0;
    for b in bins_ascending {
        if rem == 0 {
            break;
        }
        if b.reserve_x == 0 {
            continue;
        }
        // Y needed to buy out all X in the bin (rounded up — the trader pays).
        let y_all = mul_div_ceil(b.reserve_x as u128, b.price_x64, Q64);
        if rem >= y_all {
            x_out += b.reserve_x as u128;
            rem -= y_all;
        } else {
            x_out += mul_div_floor(rem, Q64, b.price_x64);
            break;
        }
    }
    sat_u64(x_out)
}

/// Sell X for Y (price falls): bins given descending from the active one downward.
/// Returns the amount of Y out.
pub fn swap_x_for_y(bins_descending: &[Bin], amount_x: u64, fee_bps: u32) -> u64 {
    let mut rem = after_fee(amount_x, fee_bps);
    let mut y_out: u128 = 0;
    for b in bins_descending {
        if rem == 0 {
            break;
        }
        if b.reserve_y == 0 {
            continue;
        }
        // X needed to drain the Y from the bin (rounded up).
        let x_all = mul_div_ceil(b.reserve_y as u128, Q64, b.price_x64);
        if rem >= x_all {
            y_out += b.reserve_y as u128;
            rem -= x_all;
        } else {
            y_out += mul_div_floor(rem, b.price_x64, Q64);
            break;
        }
    }
    sat_u64(y_out)
}

/// Same pass as [`swap_x_for_y`], with the reserves written back. Returns the new active bin index.
/// `None` when the swap touches no reserve.
pub fn apply_x_for_y(bins: &mut [Bin], active: usize, amount_x: u64, fee_bps: u32) -> Option<usize> {
    if active >= bins.len() {
        return None;
    }
    let mut rem = after_fee(amount_x, fee_bps);
    if rem == 0 {
        return None;
    }
    let mut moved = false;
    let mut new_active = active;
    for i in (0..=active).rev() {
        if rem == 0 {
            break;
        }
        if bins[i].reserve_y == 0 || bins[i].price_x64 == 0 {
            continue;
        }
        let x_all = mul_div_ceil(bins[i].reserve_y as u128, Q64, bins[i].price_x64);
        if x_all == 0 {
            continue;
        }
        if rem >= x_all {
            bins[i].reserve_x = bins[i].reserve_x.saturating_add(sat_u64(x_all));
            bins[i].reserve_y = 0;
            rem -= x_all;
            new_active = i.saturating_sub(1);
            // An exhausted lowest bin stays active.
            if i == 0 {
                new_active = 0;
            }
        } else {
            let y_out = mul_div_floor(rem, bins[i].price_x64, Q64).min(bins[i].reserve_y as u128);
            if y_out == 0 {
                break;
            }
            bins[i].reserve_y -= y_out as u64;
            bins[i].reserve_x = bins[i].reserve_x.saturating_add(sat_u64(rem));
            rem = 0;
            new_active = i;
        }
        moved = true;
    }
    if moved { Some(new_active) } else { None }
}

/// Same pass as [`swap_y_for_x`], with the reserves written back. Returns the new active bin index.
pub fn apply_y_for_x(bins: &mut [Bin], active: usize, amount_y: u64, fee_bps: u32) -> Option<usize> {
    if active >= bins.len() {
        return None;
    }
    let mut rem = after_fee(amount_y, fee_bps);
    if rem == 0 {
        return None;
    }
    let mut moved = false;
    let mut new_active = active;
    for i in active..bins.len() {
        if rem == 0 {
            break;
        }
        if bins[i].reserve_x == 0 || bins[i].price_x64 == 0 {
            continue;
        }
        let y_all = mul_div_ceil(bins[i].reserve_x as u128, bins[i].price_x64, Q64);
        if y_all == 0 {
            continue;
        }
        if rem >= y_all {
            bins[i].reserve_y = bins[i].reserve_y.saturating_add(sat_u64(y_all));
            bins[i].reserve_x = 0;
            rem -= y_all;
            new_active = (i + 1).min(bins.len() - 1);
        } else {
            let x_out = mul_div_floor(rem, Q64, bins[i].price_x64).min(bins[i].reserve_x as u128);
            if x_out == 0 {
                break;
            }
            bins[i].reserve_x -= x_out as u64;
            bins[i].reserve_y = bins[i].reserve_y.saturating_add(sat_u64(rem));
            rem = 0;
            new_active = i;
        }
        moved = true;
    }
    if moved { Some(new_active) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(price_x64: u128, rx: u64, ry: u64) -> Bin {
        Bin { price_x64, reserve_x: rx, reserve_y: ry }
    }

    #[test]
    fn single_bin_price_one() {
        // 3 bins at price 1, reserves 1e6/1e6; input 5e5, fee 0.30%.
        let bins = [bin(Q64, 1_000_000, 1_000_000); 3];
        assert_eq!(swap_y_for_x(&bins, 500_000, 30), 498_500);
        assert_eq!(swap_x_for_y(&bins, 500_000, 30), 498_500);
    }

    #[test]
    fn crosses_two_bins() {
        let bins = [bin(Q64, 1_000_000, 1_000_000); 3];
        // 1.5e6 after fee = 1,495,500; the first bin has 1e6 X, so it moves on to the second.
        assert_eq!(swap_y_for_x(&bins, 1_500_000, 30), 1_495_500);
    }

    #[test]
    fn rising_prices_consume_in_order() {
        // Prices 1.00 / 1.01 / 1.0201, each bin has 1e6 X. Input 2e6 Y, no fee.
        let p0 = Q64;
        let p1 = mul_div_floor(101, Q64, 100);
        let p2 = mul_div_floor(10_201, Q64, 10_000);
        let bins = [bin(p0, 1_000_000, 0), bin(p1, 1_000_000, 0), bin(p2, 1_000_000, 0)];
        assert_eq!(swap_y_for_x(&bins, 2_000_000, 0), 1_990_099);
    }

    #[test]
    fn empty_and_zero() {
        let bins = [bin(Q64, 0, 0)];
        assert_eq!(swap_y_for_x(&bins, 1_000, 0), 0);
        assert_eq!(swap_y_for_x(&[], 1_000, 0), 0);
    }

    #[test]
    fn apply_moves_the_same_amount_the_quote_named() {
        let original = [bin(Q64, 1_000_000, 1_000_000)];
        let quoted = swap_x_for_y(&original, 500_000, 30);
        let mut bins = original;
        let active = apply_x_for_y(&mut bins, 0, 500_000, 30).unwrap();
        assert_eq!(active, 0);
        assert_eq!(original[0].reserve_y - bins[0].reserve_y, quoted);
        assert!(bins[0].reserve_x > original[0].reserve_x);
    }
}
