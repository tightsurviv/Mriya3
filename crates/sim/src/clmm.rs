//! CLMM simulator (concentrated liquidity, Uniswap v3 / Raydium CLMM / Orca Whirlpool model).
//!
//! **Scope of the first version — a deliberate design decision.** Pricing is computed in f64 within
//! a single active liquidity range (no tick crossing). This is enough for the engine to
//! **detect opportunities and pick the size** of a backrun (it usually fits within one range).
//! Capital safety is guaranteed by the on-chain program, which computes profit in integer arithmetic
//! and **reverts the transaction if profit ≤ threshold** — an f64 rounding error can at most result in
//! a missed quote (cost = fee), never a capital loss.
//!
//! To do (once we connect real pools and a build with tests on data):
//! - exact integer arithmetic (Uniswap FullMath, 512-bit) for lamport-level agreement,
//! - tick crossing with a list of initialized ticks (liquidity_net).

/// Active CLMM liquidity range.
///
/// `sqrt_price` = √(price), where price = token1/token0 (token1 per 1 token0).
/// `liquidity` = L in pool units. `fee_bps` — pool fee.
#[derive(Clone, Copy, Debug)]
pub struct ClmmRange {
    pub sqrt_price: f64,
    pub liquidity: f64,
    pub fee_bps: u32,
}

impl ClmmRange {
    pub fn price(&self) -> f64 {
        self.sqrt_price * self.sqrt_price
    }

    /// Swap result within one range: (output amount, new sqrt_price).
    ///
    /// `zero_for_one = true`: you put in token0, price falls, you get token1.
    /// `zero_for_one = false`: you put in token1, price rises, you get token0.
    pub fn swap(&self, amount_in: u64, zero_for_one: bool) -> (u64, f64) {
        let l = self.liquidity;
        let sp = self.sqrt_price;
        if l <= 0.0 || amount_in == 0 {
            return (0, sp);
        }
        let ain = amount_in as f64 * (10_000 - self.fee_bps) as f64 / 10_000.0;
        if zero_for_one {
            // token0 in → new sqrtP = L·sp / (L + ain·sp); token1 out = L·(sp − sp')
            let new_sp = l * sp / (l + ain * sp);
            let out = l * (sp - new_sp);
            (floor_u64(out), new_sp)
        } else {
            // token1 in → new sqrtP = sp + ain/L; token0 out = L·(sp' − sp)/(sp·sp')
            let new_sp = sp + ain / l;
            let out = l * (new_sp - sp) / (sp * new_sp);
            (floor_u64(out), new_sp)
        }
    }

    pub fn amount_out(&self, amount_in: u64, zero_for_one: bool) -> u64 {
        self.swap(amount_in, zero_for_one).0
    }
}

fn floor_u64(x: f64) -> u64 {
    if x <= 0.0 {
        0
    } else if x >= u64::MAX as f64 {
        u64::MAX
    } else {
        x.floor() as u64
    }
}

/// Exact (integer) CLMM within one liquidity range — Uniswap v3 / Raydium CLMM /
/// Whirlpool model, lamport-exact. `sqrt_price_x64` = √(raw price)·2^64, `liquidity` = L.
#[derive(Clone, Copy, Debug)]
pub struct ClmmExact {
    pub sqrt_price_x64: u128,
    pub liquidity: u128,
    pub fee_bps: u32,
}

const Q64: u128 = 1u128 << 64;

impl ClmmExact {
    /// Swap within one range: (output amount, new sqrt_price_x64). No tick crossing.
    pub fn swap(&self, amount_in: u64, zero_for_one: bool) -> (u64, u128) {
        let (l, sp) = (self.liquidity, self.sqrt_price_x64);
        if l == 0 || amount_in == 0 {
            return (0, sp);
        }
        let ain = amount_in as u128 * (10_000 - self.fee_bps) as u128 / 10_000;
        if zero_for_one {
            let sp2 = next_sqrt_from_amount0(sp, l, ain); // token0 in → price falls (round up)
            (sat(amount1_delta(sp2, sp, l)), sp2) // token1 out (floor)
        } else {
            let sp2 = next_sqrt_from_amount1(sp, l, ain); // token1 in → price rises
            (sat(amount0_delta(sp, sp2, l)), sp2) // token0 out (floor)
        }
    }

    pub fn amount_out(&self, amount_in: u64, zero_for_one: bool) -> u64 {
        self.swap(amount_in, zero_for_one).0
    }

    /// Swap with **tick crossing**. `ticks` = initialized ticks (index, liquidity_net),
    /// sorted ascending by index. Per-step math is exact; tick boundaries are computed
    /// from `sqrt_price_at_tick` (f64 — to be refined with an exact getSqrtRatioAtTick). `current_tick` =
    /// the pool's current tick.
    pub fn swap_crossing(&self, amount_in: u64, zero_for_one: bool, current_tick: i32, ticks: &[(i32, i128)]) -> u64 {
        self.swap_crossing_state(amount_in, zero_for_one, current_tick, ticks).amount_out
    }

    /// The same swap as [`Self::swap_crossing`], plus the state the loop already computes.
    ///
    /// After fully crossing tick `T` downward the cursor lands on `T - 1`, upward on `T + 1`.
    /// The next swap's filter (`t < cursor` / `t > cursor`) does not add `liquidity_net` a second time
    /// in the same direction and adds it on the way back. Works when adjacent initialized ticks
    /// are at least 2 apart; with a gap of 1 the cursor lands on the next tick.
    pub fn swap_crossing_state(
        &self,
        amount_in: u64,
        zero_for_one: bool,
        current_tick: i32,
        ticks: &[(i32, i128)],
    ) -> ClmmCursor {
        let mut l = self.liquidity;
        let mut sp = self.sqrt_price_x64;
        let mut tick_now = current_tick;
        if l == 0 || amount_in == 0 {
            return ClmmCursor { amount_out: 0, sqrt_price_x64: sp, liquidity: l, current_tick: tick_now };
        }
        let mut ain = amount_in as u128 * (10_000 - self.fee_bps) as u128 / 10_000;
        let mut out: u128 = 0;

        if zero_for_one {
            let mut below: Vec<&(i32, i128)> = ticks.iter().filter(|(t, _)| *t < current_tick).collect();
            below.sort_by(|a, b| b.0.cmp(&a.0)); // descending
            for (tick, net) in below {
                if ain == 0 || l == 0 {
                    break;
                }
                let sp_t = sqrt_price_at_tick(*tick);
                let in_to = amount0_delta(sp_t, sp, l);
                if ain >= in_to {
                    out += amount1_delta(sp_t, sp, l);
                    ain -= in_to;
                    sp = sp_t;
                    l = cross_down(l, *net);
                    tick_now = tick.saturating_sub(1);
                } else {
                    let sp_new = next_sqrt_from_amount0(sp, l, ain);
                    out += amount1_delta(sp_new, sp, l);
                    sp = sp_new;
                    ain = 0;
                    break;
                }
            }
            if ain > 0 && l > 0 {
                let sp_new = next_sqrt_from_amount0(sp, l, ain);
                out += amount1_delta(sp_new, sp, l);
                sp = sp_new;
            }
        } else {
            let mut above: Vec<&(i32, i128)> = ticks.iter().filter(|(t, _)| *t > current_tick).collect();
            above.sort_by(|a, b| a.0.cmp(&b.0)); // ascending
            for (tick, net) in above {
                if ain == 0 || l == 0 {
                    break;
                }
                let sp_t = sqrt_price_at_tick(*tick);
                let in_to = amount1_delta(sp, sp_t, l);
                if ain >= in_to {
                    out += amount0_delta(sp, sp_t, l);
                    ain -= in_to;
                    sp = sp_t;
                    l = cross_up(l, *net);
                    tick_now = tick.saturating_add(1);
                } else {
                    let sp_new = next_sqrt_from_amount1(sp, l, ain);
                    out += amount0_delta(sp, sp_new, l);
                    sp = sp_new;
                    ain = 0;
                    break;
                }
            }
            if ain > 0 && l > 0 {
                let sp_new = next_sqrt_from_amount1(sp, l, ain);
                out += amount0_delta(sp, sp_new, l);
                sp = sp_new;
            }
        }
        ClmmCursor { amount_out: sat(out), sqrt_price_x64: sp, liquidity: l, current_tick: tick_now }
    }
}

/// CLMM state after a swap with tick crossing. The output amount is the same number as
/// [`ClmmExact::swap_crossing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClmmCursor {
    pub amount_out: u64,
    pub sqrt_price_x64: u128,
    pub liquidity: u128,
    pub current_tick: i32,
}

/// √(raw price)·2^64 for a given tick: 1.0001^(tick/2)·2^64 (f64; the exact
/// getSqrtRatioAtTick will come later).
pub fn sqrt_price_at_tick(tick: i32) -> u128 {
    let v = 1.0001f64.powf(tick as f64 / 2.0) * 2f64.powi(64);
    if v <= 0.0 {
        1
    } else if v >= u128::MAX as f64 {
        u128::MAX
    } else {
        v as u128
    }
}

fn cross_down(l: u128, net: i128) -> u128 {
    let r = l as i128 - net;
    if r < 0 { 0 } else { r as u128 }
}
fn cross_up(l: u128, net: i128) -> u128 {
    let r = l as i128 + net;
    if r < 0 { 0 } else { r as u128 }
}

fn sat(v: u128) -> u64 {
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

// sqrtP' = ceil( (L<<64)·sqrtP / ((L<<64) + amt·sqrtP) )  — token0 added, price down.
fn next_sqrt_from_amount0(sp: u128, l: u128, amt: u128) -> u128 {
    use crate::u256::{mul_div_ceil, widening_mul, U256};
    if amt == 0 {
        return sp;
    }
    let num = U256::shl64(l);
    let den = num.add(widening_mul(amt, sp));
    mul_div_ceil(num, U256::from_u128(sp), den)
}

// sqrtP' = sqrtP + floor(amt·2^64 / L)  — token1 added, price up.
fn next_sqrt_from_amount1(sp: u128, l: u128, amt: u128) -> u128 {
    use crate::u256::{mul_div_floor, U256};
    sp.wrapping_add(mul_div_floor(U256::from_u128(amt), U256::from_u128(Q64), U256::from_u128(l)))
}

// |token1| between prices: floor( L·(sb−sa) / 2^64 ).
fn amount1_delta(sa: u128, sb: u128, l: u128) -> u128 {
    use crate::u256::{mul_div_floor, U256};
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    mul_div_floor(U256::from_u128(l), U256::from_u128(sb - sa), U256::from_u128(Q64))
}

// |token0| between prices: floor( (L<<64)·(sb−sa) / (sb·sa) ).
fn amount0_delta(sa: u128, sb: u128, l: u128) -> u128 {
    use crate::u256::{mul_div_floor, widening_mul, U256};
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    mul_div_floor(U256::shl64(l), U256::from_u128(sb - sa), widening_mul(sb, sa))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_reference_values() {
        // Values computed independently in arbitrary-precision arithmetic.
        let r = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 1_000_000_000, fee_bps: 30 };
        let (out, sp2) = r.swap(1_000_000, true);
        assert_eq!(out, 996_006);
        assert_eq!(sp2, 18_428_370_987_834_680_440);

        let (out2, sp2b) = r.swap(1_000_000, false);
        assert_eq!(out2, 996_006);
        assert_eq!(sp2b, 18_465_135_477_551_040_038);

        // Large L: exact result 996,999 (f64 lost the low digits here).
        let big = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 5_000_000_000_000_000_000, fee_bps: 30 };
        assert_eq!(big.swap(1_000_000, true).0, 996_999);
    }

    #[test]
    fn crossing_matches_single_range_without_ticks() {
        let e = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 1_000_000_000_000, fee_bps: 0 };
        // No ticks (or tick not reached) = single-range result.
        assert_eq!(e.swap_crossing(1_000, true, 0, &[]), e.amount_out(1_000, true));
        // Small swap with a distant tick — tick not crossed, same result.
        assert_eq!(e.swap_crossing(1_000, true, 0, &[(-100_000, 5_000_000_000)]), e.amount_out(1_000, true));
    }

    #[test]
    fn crossing_reduces_output_when_liquidity_drops() {
        // Large swap down through a tick with positive liquidity_net → after crossing L drops,
        // so the output is SMALLER than with constant liquidity (single-range).
        let e = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 1_000_000_000_000, fee_bps: 0 };
        let big = 10_000_000_000u64;
        let crossing = e.swap_crossing(big, true, 0, &[(-100, 800_000_000_000)]);
        let single = e.amount_out(big, true);
        assert!(crossing > 0);
        assert!(crossing < single, "crossing {crossing} should be < single {single}");
    }

    #[test]
    fn crossing_state_keeps_the_amount_and_moves_the_cursor() {
        let e = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 1_000_000_000_000, fee_bps: 0 };
        let ticks = [(-100, 800_000_000_000i128)];
        let big = 10_000_000_000u64;
        let state = e.swap_crossing_state(big, true, 0, &ticks);
        assert_eq!(state.amount_out, e.swap_crossing(big, true, 0, &ticks));
        assert!(state.sqrt_price_x64 < e.sqrt_price_x64);
        assert_eq!(state.liquidity, 200_000_000_000);
        assert_eq!(state.current_tick, -101);

        let small = e.swap_crossing_state(1_000, true, 0, &ticks);
        assert_eq!(small.amount_out, e.amount_out(1_000, true));
        assert!(small.sqrt_price_x64 < e.sqrt_price_x64);
        assert_eq!(small.liquidity, e.liquidity);
        assert_eq!(small.current_tick, 0);

        // The second step in the same direction starts past the tick, so it does not subtract net a second time.
        let again = ClmmExact {
            sqrt_price_x64: state.sqrt_price_x64,
            liquidity: state.liquidity,
            fee_bps: 0,
        };
        let step2 = again.swap_crossing_state(1_000, true, state.current_tick, &ticks);
        assert_eq!(step2.liquidity, state.liquidity);
        assert_eq!(step2.current_tick, state.current_tick);
    }

    #[test]
    fn exact_matches_f64_on_normal_values() {
        let e = ClmmExact { sqrt_price_x64: 1u128 << 64, liquidity: 1_000_000_000, fee_bps: 30 };
        let f = ClmmRange { sqrt_price: 1.0, liquidity: 1e9, fee_bps: 30 };
        let de = e.amount_out(1_000_000, true) as i64;
        let df = f.amount_out(1_000_000, true) as i64;
        assert!((de - df).abs() <= 2);
    }

    #[test]
    fn reference_near_price_one() {
        // sqrtP=1 (price 1), L=1e9, input 1e6, fee 0.30% → ~996,006 (matches the integer reference).
        let r = ClmmRange { sqrt_price: 1.0, liquidity: 1e9, fee_bps: 30 };
        let out = r.amount_out(1_000_000, true);
        assert!((out as i64 - 996_006).abs() <= 5, "out={out}");
    }

    #[test]
    fn symmetric_both_directions_at_price_one() {
        let r = ClmmRange { sqrt_price: 1.0, liquidity: 1e9, fee_bps: 30 };
        let a = r.amount_out(1_000_000, true);
        let b = r.amount_out(1_000_000, false);
        assert!((a as i64 - b as i64).abs() <= 2);
    }

    #[test]
    fn huge_liquidity_is_near_lossless() {
        // L >> input → almost no slippage → output ≈ input after fee.
        // L = 1e15 is large, yet still fully precise in f64. At L ≳ 1e16 f64 loses
        // the low digits (error on the order of a few dozen) — that is the limit of the f64 version, for which
        // lamport-exact accuracy will come from integer arithmetic (512-bit) + the on-chain guard.
        let r = ClmmRange { sqrt_price: 1.0, liquidity: 1e15, fee_bps: 30 };
        let out = r.amount_out(1_000_000, true);
        assert!((out as i64 - 997_000).abs() <= 5, "out={out}");
    }

    #[test]
    fn price_moves_correct_direction() {
        let r = ClmmRange { sqrt_price: 1.0, liquidity: 1e9, fee_bps: 30 };
        let (_, sp_down) = r.swap(1_000_000, true);
        let (_, sp_up) = r.swap(1_000_000, false);
        assert!(sp_down < 1.0, "token0 in → price falls");
        assert!(sp_up > 1.0, "token1 in → price rises");
    }
}
