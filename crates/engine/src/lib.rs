//! Engine: pool registry + detection of arbitrage opportunities and size selection.
//!
//! First, fully testable version: **two-pool** arbitrage on the same pair (base ↔ token T).
//! For each pair of pools trading `base ↔ T`, we compute the route `base →(p_buy) T →(p_sell) base`,
//! pick the input size ([`sim::argmax_unimodal`]) and keep opportunities with profit > threshold.
//!
//! Profit is computed from the public pool state; the final, exact threshold is enforced by the on-chain program
//! (revert when there is no profit). Supported routes: 2-leg ([`Engine::find_two_hop`]), triangular
//! 3-leg ([`Engine::find_three_hop`]) and 4- and 5-leg cycles ([`Engine::find_cycle`]).
//! In the Mriya window, lengths 107/112/117/122 B are n = 2/3/4/5 (7,581/2,517/1,059/17 instructions).
//! The parser accepts n = 6; there was no such length in the window.
//! Pricing for Cp/DLMM/CLMM (with tick crossing) and Manifest books.

use std::collections::HashMap;

use sim::{argmax_unimodal, CpPool};

/// How many full 4- or 5-leg cycles we price in one call.
/// Above this count we return what we have already computed. This is a limit of this engine,
/// not a threshold from the Mriya measurement.
const MAX_CYCLE_EVALS: usize = 20_000;

pub mod encode;

pub type TokenId = [u8; 32];
pub type PoolId = [u8; 32];

/// Pool pricing model.
#[derive(Clone, Debug)]
pub enum Kind {
    /// Constant product (Raydium v4/CPMM, Pump AMM, Meteora DAMM): reserves of tokens a and b.
    Cp { res_a: u64, res_b: u64, fee_bps: u32 },
    /// DLMM (Meteora): token_a = X, token_b = Y; `bins` ascending by price (Y per X),
    /// `active` = index of the active bin. Bin price: `price_x64` (Y per 1 X, Q64.64).
    Dlmm { bins: Vec<sim::dlmm::Bin>, active: usize, fee_bps: u32 },
    /// CLMM (Raydium/Whirlpool): token_a = token0, token_b = token1. Exact (integer) pricing;
    /// when `ticks` is non-empty — with tick crossing. `sqrt_price_x64` = √(raw price)·2^64,
    /// `current_tick` = current tick, `ticks` = (index, liquidity_net) of initialized ticks.
    Clmm { sqrt_price_x64: u128, liquidity: u128, fee_bps: u32, current_tick: i32, ticks: Vec<(i32, i128)> },
    /// Manifest book. `token_a` = base, `token_b` = quote. Both sides from the best price.
    /// `a_to_b` sells base into the bids. The other side pays quote and takes base from the asks.
    Book { bids: Vec<sim::book::Level>, asks: Vec<sim::book::Level> },
}

/// Pool trading the pair (token_a, token_b).
#[derive(Clone, Debug)]
pub struct Pool {
    pub id: PoolId,
    pub token_a: TokenId,
    pub token_b: TokenId,
    pub kind: Kind,
}

impl Pool {
    /// Swap quote: how much output token for `amount_in`, in the a→b direction (when `a_to_b`).
    pub fn quote(&self, amount_in: u64, a_to_b: bool) -> u64 {
        match &self.kind {
            Kind::Cp { res_a, res_b, fee_bps } => {
                let pool = if a_to_b {
                    CpPool::new(*res_a, *res_b, *fee_bps)
                } else {
                    CpPool::new(*res_b, *res_a, *fee_bps)
                };
                sim::amount_out(amount_in, pool)
            }
            Kind::Dlmm { bins, active, fee_bps } => {
                if *active >= bins.len() {
                    return 0;
                }
                if a_to_b {
                    // X→Y: sell X, price falls — bins from the active one downward.
                    let desc: Vec<sim::dlmm::Bin> = bins[..=*active].iter().rev().copied().collect();
                    sim::dlmm::swap_x_for_y(&desc, amount_in, *fee_bps)
                } else {
                    // Y→X: buy X, price rises — bins from the active one upward.
                    sim::dlmm::swap_y_for_x(&bins[*active..], amount_in, *fee_bps)
                }
            }
            Kind::Clmm { sqrt_price_x64, liquidity, fee_bps, current_tick, ticks } => {
                // Exact (integer) pricing; a_to_b = zero_for_one.
                let r = sim::clmm::ClmmExact {
                    sqrt_price_x64: *sqrt_price_x64,
                    liquidity: *liquidity,
                    fee_bps: *fee_bps,
                };
                if ticks.is_empty() {
                    r.amount_out(amount_in, a_to_b)
                } else {
                    r.swap_crossing(amount_in, a_to_b, *current_tick, ticks)
                }
            }
            Kind::Book { bids, asks } => {
                if a_to_b {
                    let mut side = bids.clone();
                    sim::book::sell_base(&mut side, amount_in).unwrap_or(0)
                } else {
                    let mut side = asks.clone();
                    sim::book::buy_base(&mut side, amount_in).unwrap_or(0)
                }
            }
        }
    }

    /// The other token relative to the given one (or `None` if the pool does not contain it).
    pub fn other(&self, t: &TokenId) -> Option<TokenId> {
        if &self.token_a == t {
            Some(self.token_b)
        } else if &self.token_b == t {
            Some(self.token_a)
        } else {
            None
        }
    }

    /// The other token relative to the base (or `None` if the pool does not contain the base).
    pub fn other_token(&self, base: &TokenId) -> Option<TokenId> {
        self.other(base)
    }

    /// Forward quote: if the pool contains `from`, returns `(output token, amount)`.
    pub fn forward(&self, from: &TokenId, amount_in: u64) -> Option<(TokenId, u64)> {
        let to = self.other(from)?;
        Some((to, self.quote(amount_in, &self.token_a == from)))
    }

    fn base_to_other(&self, base: &TokenId, amount_in: u64) -> u64 {
        self.quote(amount_in, &self.token_a == base)
    }

    fn other_to_base(&self, base: &TokenId, amount_in: u64) -> u64 {
        self.quote(amount_in, &self.token_a != base)
    }

    /// Copy of the pool after a swap. The original's reserves stay. `None` when the swap moves nothing.
    /// CLMM with ticks stores the sqrt, liquidity and cursor, which the same swap as [`Self::quote`] computes.
    pub fn after_swap(&self, amount_in: u64, a_to_b: bool) -> Option<Pool> {
        if amount_in == 0 {
            return None;
        }
        let mut next = self.clone();
        match &mut next.kind {
            Kind::Cp { res_a, res_b, fee_bps } => {
                let (rin, rout) = if a_to_b { (*res_a, *res_b) } else { (*res_b, *res_a) };
                let out = sim::amount_out(amount_in, CpPool::new(rin, rout, *fee_bps));
                if out == 0 || out > rout {
                    return None;
                }
                if a_to_b {
                    *res_a = res_a.checked_add(amount_in)?;
                    *res_b -= out;
                } else {
                    *res_b = res_b.checked_add(amount_in)?;
                    *res_a -= out;
                }
            }
            Kind::Dlmm { bins, active, fee_bps } => {
                let moved = if a_to_b {
                    sim::dlmm::apply_x_for_y(bins, *active, amount_in, *fee_bps)?
                } else {
                    sim::dlmm::apply_y_for_x(bins, *active, amount_in, *fee_bps)?
                };
                *active = moved;
            }
            Kind::Clmm { sqrt_price_x64, liquidity, fee_bps, current_tick, ticks } => {
                let range = sim::clmm::ClmmExact {
                    sqrt_price_x64: *sqrt_price_x64,
                    liquidity: *liquidity,
                    fee_bps: *fee_bps,
                };
                if ticks.is_empty() {
                    let (out, next_sqrt) = range.swap(amount_in, a_to_b);
                    if out == 0 || next_sqrt == *sqrt_price_x64 {
                        return None;
                    }
                    *sqrt_price_x64 = next_sqrt;
                } else {
                    let moved = range.swap_crossing_state(amount_in, a_to_b, *current_tick, ticks);
                    if moved.amount_out == 0
                        || (moved.sqrt_price_x64 == *sqrt_price_x64 && moved.liquidity == *liquidity)
                    {
                        return None;
                    }
                    *sqrt_price_x64 = moved.sqrt_price_x64;
                    *liquidity = moved.liquidity;
                    *current_tick = moved.current_tick;
                }
            }
            Kind::Book { bids, asks } => {
                let filled = if a_to_b {
                    sim::book::sell_base(bids, amount_in)
                } else {
                    sim::book::buy_base(asks, amount_in)
                };
                if filled.is_none() {
                    return None;
                }
            }
        }
        Some(next)
    }

    /// Base reserve in the pool (to cap the input size).
    fn base_reserve(&self, base: &TokenId) -> u64 {
        match &self.kind {
            Kind::Cp { res_a, res_b, .. } => {
                if &self.token_a == base {
                    *res_a
                } else {
                    *res_b
                }
            }
            Kind::Dlmm { bins, .. } => {
                // base = token_a → X (sum reserve_x), otherwise token_b → Y (reserve_y).
                let a_is_base = &self.token_a == base;
                bins.iter()
                    .map(|b| if a_is_base { b.reserve_x } else { b.reserve_y } as u128)
                    .sum::<u128>()
                    .min(u64::MAX as u128) as u64
            }
            // CLMM has no simple "reserves"; the size cap is left to `max_input`.
            Kind::Clmm { .. } => u64::MAX,
            Kind::Book { bids, asks } => {
                if &self.token_a == base {
                    bids.iter().fold(0u64, |sum, level| sum.saturating_add(level.base))
                } else if &self.token_b == base {
                    sim::book::quote_depth(asks)
                } else {
                    0
                }
            }
        }
    }
}

/// Detected opportunity: buy token T on `buy_pool`, sell on `sell_pool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opportunity {
    pub token: TokenId,
    pub buy_pool: PoolId,
    pub sell_pool: PoolId,
    pub amount_in: u64,
    pub profit: u64,
}

/// Detected triangular opportunity: `base →(p0) T →(p1) U →(p2) base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriOpportunity {
    /// Intermediate tokens of the route: `[T, U]`.
    pub tokens: [TokenId; 2],
    /// Pools in leg order: `base→T`, `T→U`, `U→base`.
    pub pools: [PoolId; 3],
    pub amount_in: u64,
    pub profit: u64,
}

/// Cycle `base → … → base` with 4 or 5 legs.
///
/// `tokens` are the intermediate tokens, without the base at the ends. `pools.len() == tokens.len() + 1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CycleOpportunity {
    pub tokens: Vec<TokenId>,
    pub pools: Vec<PoolId>,
    pub amount_in: u64,
    pub profit: u64,
}

/// A user swap that is yet to land. We apply it to a copy of the state, not to the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VictimSwap {
    pub pool: PoolId,
    /// `true`: token_a goes in, token_b comes out.
    pub a_to_b: bool,
    pub amount_in: u64,
}

/// Engine with a pool registry.
#[derive(Clone)]
pub struct Engine {
    pub base: TokenId,
    pub pools: Vec<Pool>,
    /// Upper limit on the input (e.g. available base capital).
    pub max_input: u64,
    /// Minimum profit in base units (should cover fee + tip). Lower → skip.
    pub min_profit: u64,
}

impl Engine {
    pub fn new(base: TokenId, max_input: u64, min_profit: u64) -> Self {
        Self { base, pools: Vec::new(), max_input, min_profit }
    }

    pub fn add_pool(&mut self, p: Pool) {
        self.pools.push(p);
    }

    /// Copy of the engine after the user's swap. The registry we call on stays as it was before the swap.
    pub fn after_victim(&self, swap: VictimSwap) -> Option<Engine> {
        let mut next = self.clone();
        let pool = next.pools.iter_mut().find(|p| p.id == swap.pool)?;
        *pool = pool.after_swap(swap.amount_in, swap.a_to_b)?;
        Some(next)
    }

    /// Copy after several swaps, in the given order. A swap whose pool is not in the registry is skipped.
    pub fn after_victims(&self, swaps: &[VictimSwap]) -> Option<Engine> {
        let mut next = self.clone();
        let mut applied = false;
        for swap in swaps {
            if let Some(updated) = next.after_victim(*swap) {
                next = updated;
                applied = true;
            }
        }
        if applied { Some(next) } else { None }
    }

    /// Finds two-pool opportunities, sorted by profit descending.
    pub fn find_two_hop(&self) -> Vec<Opportunity> {
        let mut out = Vec::new();
        let n = self.pools.len();
        for i in 0..n {
            let pi = &self.pools[i];
            let ti = match pi.other_token(&self.base) {
                Some(t) => t,
                None => continue,
            };
            for j in 0..n {
                if i == j {
                    continue;
                }
                let pj = &self.pools[j];
                match pj.other_token(&self.base) {
                    Some(tj) if tj == ti => {}
                    _ => continue,
                }
                // Route: base →(pi) T →(pj) base.
                let cap = self.max_input.min(pi.base_reserve(&self.base));
                let f = |x: u64| -> i128 {
                    let t = pi.base_to_other(&self.base, x);
                    let back = pj.other_to_base(&self.base, t);
                    back as i128 - x as i128
                };
                let opt = argmax_unimodal(cap, f);
                if opt.profit > self.min_profit as i128 {
                    out.push(Opportunity {
                        token: ti,
                        buy_pool: pi.id,
                        sell_pool: pj.id,
                        amount_in: opt.amount_in,
                        profit: opt.profit as u64,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.profit.cmp(&a.profit));
        out
    }

    /// Finds triangular (3-leg) opportunities `base→T→U→base`, sorted by profit descending.
    /// `U ≠ base` (otherwise it is a 2-leg route), all three pools distinct.
    pub fn find_three_hop(&self) -> Vec<TriOpportunity> {
        let mut out = Vec::new();
        let n = self.pools.len();
        for i in 0..n {
            let pi = &self.pools[i];
            let t = match pi.other_token(&self.base) {
                Some(t) => t,
                None => continue,
            };
            for j in 0..n {
                if j == i {
                    continue;
                }
                let pj = &self.pools[j];
                let u = match pj.other(&t) {
                    Some(u) if u != self.base => u,
                    _ => continue, // pj must contain T, and U ≠ base
                };
                for k in 0..n {
                    if k == i || k == j {
                        continue;
                    }
                    let pk = &self.pools[k];
                    if pk.other(&u) != Some(self.base) {
                        continue; // pk must connect U to the base
                    }
                    let cap = self.max_input.min(pi.base_reserve(&self.base));
                    let f = |x: u64| -> i128 {
                        let a = pi.quote(x, pi.token_a == self.base); // base→T
                        let b = pj.quote(a, pj.token_a == t); // T→U
                        let c = pk.quote(b, pk.token_a == u); // U→base
                        c as i128 - x as i128
                    };
                    let opt = argmax_unimodal(cap, f);
                    if opt.profit > self.min_profit as i128 {
                        out.push(TriOpportunity {
                            tokens: [t, u],
                            pools: [pi.id, pj.id, pk.id],
                            amount_in: opt.amount_in,
                            profit: opt.profit as u64,
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| b.profit.cmp(&a.profit));
        out
    }

    /// Cycle with exactly `hops` legs. In the Mriya window 4 and 5 occur.
    /// Any other length returns an empty list. A pool appears once in a cycle,
    /// and the base returns only on the last leg.
    pub fn find_cycle(&self, hops: usize) -> Vec<CycleOpportunity> {
        if !(4..=5).contains(&hops) || self.pools.is_empty() {
            return Vec::new();
        }
        let mut adj: HashMap<TokenId, Vec<(usize, TokenId)>> = HashMap::new();
        for (i, pool) in self.pools.iter().enumerate() {
            if pool.token_a == pool.token_b {
                continue;
            }
            adj.entry(pool.token_a).or_default().push((i, pool.token_b));
            adj.entry(pool.token_b).or_default().push((i, pool.token_a));
        }
        let mut out = Vec::new();
        let mut path_pools = Vec::with_capacity(hops);
        let mut path_tokens = Vec::with_capacity(hops - 1);
        let mut used = vec![false; self.pools.len()];
        let mut seen = 0usize;
        self.walk_cycle(hops, self.base, &adj, &mut path_pools, &mut path_tokens, &mut used, &mut out, &mut seen);
        out.sort_by(|a, b| b.profit.cmp(&a.profit).then_with(|| a.pools.cmp(&b.pools)));
        out
    }

    fn walk_cycle(
        &self,
        hops: usize,
        token: TokenId,
        adj: &HashMap<TokenId, Vec<(usize, TokenId)>>,
        path_pools: &mut Vec<usize>,
        path_tokens: &mut Vec<TokenId>,
        used: &mut [bool],
        out: &mut Vec<CycleOpportunity>,
        seen: &mut usize,
    ) {
        if *seen >= MAX_CYCLE_EVALS {
            return;
        }
        let Some(edges) = adj.get(&token) else { return };
        let edges = edges.clone();
        for (pool_idx, to) in edges {
            if used[pool_idx] {
                continue;
            }
            let last = path_pools.len() + 1 == hops;
            if last {
                if to != self.base {
                    continue;
                }
            } else if to == self.base {
                continue;
            }
            used[pool_idx] = true;
            path_pools.push(pool_idx);
            if last {
                *seen += 1;
                if let Some(op) = self.score_cycle(path_pools, path_tokens) {
                    out.push(op);
                }
            } else {
                path_tokens.push(to);
                self.walk_cycle(hops, to, adj, path_pools, path_tokens, used, out, seen);
                path_tokens.pop();
            }
            path_pools.pop();
            used[pool_idx] = false;
            if *seen >= MAX_CYCLE_EVALS {
                break;
            }
        }
    }

    fn score_cycle(&self, pool_idxs: &[usize], tokens: &[TokenId]) -> Option<CycleOpportunity> {
        let first = &self.pools[*pool_idxs.first()?];
        let cap = self.max_input.min(first.base_reserve(&self.base));
        let opt = argmax_unimodal(cap, |x| {
            let back = self.quote_along(pool_idxs, x);
            back as i128 - x as i128
        });
        if opt.profit > self.min_profit as i128 {
            Some(CycleOpportunity {
                tokens: tokens.to_vec(),
                pools: pool_idxs.iter().map(|&i| self.pools[i].id).collect(),
                amount_in: opt.amount_in,
                profit: opt.profit as u64,
            })
        } else {
            None
        }
    }

    fn quote_along(&self, pool_idxs: &[usize], amount_in: u64) -> u64 {
        let mut token = self.base;
        let mut amount = amount_in;
        for &idx in pool_idxs {
            let pool = &self.pools[idx];
            let Some(next) = pool.other(&token) else { return 0 };
            amount = pool.quote(amount, pool.token_a == token);
            token = next;
        }
        if token != self.base {
            0
        } else {
            amount
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> [u8; 32] {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    fn cp_pool(pid: u8, a: u8, b: u8, res_a: u64, res_b: u64) -> Pool {
        Pool { id: id(pid), token_a: id(a), token_b: id(b), kind: Kind::Cp { res_a, res_b, fee_bps: 30 } }
    }

    #[test]
    fn finds_two_pool_arbitrage() {
        let base = id(1); // BASE
        let mut e = Engine::new(base, 500_000, 0);
        // pool 10: (BASE, T) 1e6/2e6 → T cheap (lots of T per BASE)
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 2_000_000));
        // pool 11: (BASE, T) 2e6/1e6 → T expensive (lots of BASE per T)
        e.add_pool(cp_pool(11, 1, 2, 2_000_000, 1_000_000));
        let ops = e.find_two_hop();
        assert!(!ops.is_empty(), "there should be an opportunity");
        let best = ops[0];
        assert_eq!(best.token, id(2));
        assert_eq!(best.buy_pool, id(10)); // buy T where it is cheap
        assert_eq!(best.sell_pool, id(11));
        assert!(best.profit > 0 && best.amount_in > 0);
    }

    #[test]
    fn clmm_tick_cross_updates_the_replica() {
        let sqrt = 1u128 << 64;
        let pool = Pool {
            id: id(10),
            token_a: id(1),
            token_b: id(2),
            kind: Kind::Clmm {
                sqrt_price_x64: sqrt,
                liquidity: 1_000_000_000_000,
                fee_bps: 0,
                current_tick: 0,
                ticks: vec![(-100, 800_000_000_000)],
            },
        };
        let next = pool.after_swap(10_000_000_000, true).expect("swap crosses the tick");
        match next.kind {
            Kind::Clmm { sqrt_price_x64, liquidity, current_tick, .. } => {
                assert!(sqrt_price_x64 < sqrt);
                assert_eq!(liquidity, 200_000_000_000);
                assert_eq!(current_tick, -101);
            }
            _ => panic!("CLMM"),
        }
        let inside = pool.after_swap(1_000, true).expect("swap within the range");
        match inside.kind {
            Kind::Clmm { sqrt_price_x64, liquidity, current_tick, .. } => {
                assert!(sqrt_price_x64 < sqrt);
                assert_eq!(liquidity, 1_000_000_000_000);
                assert_eq!(current_tick, 0);
            }
            _ => panic!("CLMM"),
        }
    }

    #[test]
    fn victim_swap_creates_the_arb_on_a_copy() {
        let base = id(1);
        let mut e = Engine::new(base, 500_000, 0);
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 1_000_000));
        e.add_pool(cp_pool(11, 1, 2, 1_000_000, 1_000_000));
        assert!(e.find_two_hop().is_empty(), "balanced pools give no arbitrage");

        let post = e
            .after_victim(VictimSwap { pool: id(10), a_to_b: true, amount_in: 400_000 })
            .expect("swap goes through");
        assert!(e.find_two_hop().is_empty(), "the original stays as before the swap");
        let ops = post.find_two_hop();
        assert!(!ops.is_empty(), "there is arbitrage after the user's swap");
        assert!(ops[0].profit > 0);
    }

    #[test]
    fn no_arbitrage_between_equal_pools() {
        let base = id(1);
        let mut e = Engine::new(base, 500_000, 0);
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 1_000_000));
        e.add_pool(cp_pool(11, 1, 2, 1_000_000, 1_000_000));
        assert!(e.find_two_hop().is_empty());
    }

    #[test]
    fn min_profit_filters() {
        let base = id(1);
        let mut e = Engine::new(base, 500_000, u64::MAX); // unreachable threshold
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 2_000_000));
        e.add_pool(cp_pool(11, 1, 2, 2_000_000, 1_000_000));
        assert!(e.find_two_hop().is_empty());
    }

    #[test]
    fn ignores_pools_without_base() {
        let base = id(1);
        let mut e = Engine::new(base, 500_000, 0);
        // pair (T, U) — no base, should be skipped (and not panic).
        e.add_pool(cp_pool(10, 2, 3, 1_000_000, 2_000_000));
        e.add_pool(cp_pool(11, 2, 3, 2_000_000, 1_000_000));
        assert!(e.find_two_hop().is_empty());
    }

    #[test]
    fn finds_cp_to_dlmm_arbitrage() {
        // Core of the market: the same pair (MEME, SOL) on Pump AMM (Cp) and Meteora (DLMM).
        const Q64: u128 = 1u128 << 64;
        let meme = id(1);
        let sol = id(2);

        // Pump AMM: MEME cheap (2e6 MEME / 1e6 SOL ≈ 0.5 SOL/MEME).
        let cp = Pool {
            id: id(10),
            token_a: meme,
            token_b: sol,
            kind: Kind::Cp { res_a: 2_000_000, res_b: 1_000_000, fee_bps: 25 },
        };
        // DLMM: MEME expensive (bin price ≈ 1.0 SOL/MEME), lots of SOL to pay out.
        let dlmm = Pool {
            id: id(11),
            token_a: meme, // X
            token_b: sol,  // Y
            kind: Kind::Dlmm {
                bins: vec![sim::dlmm::Bin { price_x64: Q64, reserve_x: 0, reserve_y: 5_000_000 }],
                active: 0,
                fee_bps: 25,
            },
        };

        let mut e = Engine::new(sol, 400_000, 0);
        e.add_pool(cp);
        e.add_pool(dlmm);
        let ops = e.find_two_hop();
        assert!(!ops.is_empty(), "should find Cp↔DLMM arbitrage");
        assert_eq!(ops[0].buy_pool, id(10), "buy MEME on the cheap Cp pool");
        assert_eq!(ops[0].sell_pool, id(11), "sell MEME on the expensive DLMM");
        assert!(ops[0].profit > 0 && ops[0].amount_in > 0);
    }

    #[test]
    fn book_sell_shrinks_the_bid_and_caps_the_input() {
        let d18 = 1_000_000_000_000_000_000u128;
        let pool = Pool {
            id: id(10),
            token_a: id(1),
            token_b: id(2),
            kind: Kind::Book {
                bids: vec![sim::book::Level { price: d18 + d18 / 2, base: 1 }],
                asks: vec![sim::book::Level { price: d18, base: 50 }],
            },
        };
        assert_eq!(pool.quote(1, true), 2);
        let sold = pool.after_swap(1, true).expect("bid is consumed");
        match sold.kind {
            Kind::Book { bids, asks } => {
                assert!(bids.is_empty());
                assert_eq!(asks[0].base, 50);
            }
            _ => panic!("book"),
        }
        assert_eq!(pool.quote(20, false), 20);
        let bought = pool.after_swap(20, false).expect("ask is consumed");
        match bought.kind {
            Kind::Book { asks, .. } => assert_eq!(asks[0].base, 30),
            _ => panic!("book"),
        }

        let base = id(1);
        let mut e = Engine::new(base, 1_000_000, 0);
        e.add_pool(Pool {
            id: id(10),
            token_a: base,
            token_b: id(2),
            kind: Kind::Book {
                bids: vec![sim::book::Level { price: 3 * d18, base: 10 }],
                asks: vec![],
            },
        });
        e.add_pool(cp_pool(11, 2, 1, 1_000, 1_000_000));
        let ops = e.find_two_hop();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].buy_pool, id(10));
        assert_eq!(ops[0].amount_in, 10);
    }

    #[test]
    fn finds_triangular_arbitrage() {
        // base=1, T=2, U=3. Each leg ~2x (res 1e6/2e6) → profitable loop.
        let base = id(1);
        let mut e = Engine::new(base, 100_000, 0);
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 2_000_000)); // base→T
        e.add_pool(cp_pool(20, 2, 3, 1_000_000, 2_000_000)); // T→U
        e.add_pool(cp_pool(30, 3, 1, 1_000_000, 2_000_000)); // U→base
        let ops = e.find_three_hop();
        assert!(!ops.is_empty(), "should find triangular arbitrage");
        let best = ops[0];
        assert_eq!(best.tokens, [id(2), id(3)]);
        assert_eq!(best.pools, [id(10), id(20), id(30)]);
        assert!(best.profit > 0 && best.amount_in > 0);
    }

    #[test]
    fn no_triangular_arb_on_balanced_pools() {
        // All pools 1:1 → loop without profit (negative after fees).
        let base = id(1);
        let mut e = Engine::new(base, 100_000, 0);
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 1_000_000));
        e.add_pool(cp_pool(20, 2, 3, 1_000_000, 1_000_000));
        e.add_pool(cp_pool(30, 3, 1, 1_000_000, 1_000_000));
        assert!(e.find_three_hop().is_empty());
    }

    #[test]
    fn three_hop_requires_u_distinct_from_base() {
        // Only base↔T pools (U would coincide with the base) → no 3-leg route.
        let base = id(1);
        let mut e = Engine::new(base, 100_000, 0);
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 2_000_000));
        e.add_pool(cp_pool(11, 1, 2, 2_000_000, 1_000_000));
        assert!(e.find_three_hop().is_empty(), "this is a 2-leg route, not a 3-leg one");
    }

    fn doubling_cycle(n_hops: u8) -> Engine {
        let base = id(1);
        let mut e = Engine::new(base, 100_000, 0);
        for i in 0..n_hops {
            let from = if i == 0 { 1 } else { i + 1 };
            let to = if i + 1 == n_hops { 1 } else { i + 2 };
            e.add_pool(cp_pool(10 + i, from, to, 1_000_000, 2_000_000));
        }
        e
    }

    #[test]
    fn finds_four_and_five_hop_cycles_at_measured_lengths() {
        let four = doubling_cycle(4);
        let ops = four.find_cycle(4);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].tokens, vec![id(2), id(3), id(4)]);
        assert_eq!(ops[0].pools, vec![id(10), id(11), id(12), id(13)]);
        assert!(ops[0].profit > 0 && ops[0].amount_in > 0);
        assert!(four.find_two_hop().is_empty());
        assert!(four.find_three_hop().is_empty());
        assert!(four.find_cycle(5).is_empty());
        assert!(four.find_cycle(3).is_empty());
        assert!(four.find_cycle(6).is_empty());

        let hops4 = vec![encode::PlanHop { venue: 0x03, direction: 0, n_accounts: 3, token_account_size: 0 }; 4];
        let body4 = encode::encode_route(ops[0].amount_in, 0, &hops4, &encode::RouteMeta::new(1, 0)).unwrap();
        assert_eq!(body4.len(), 117, "n = 4 is 117 B in the window");
        assert_eq!(body4[25], 4);

        let five = doubling_cycle(5);
        let ops5 = five.find_cycle(5);
        assert_eq!(ops5.len(), 1);
        assert_eq!(ops5[0].pools, vec![id(10), id(11), id(12), id(13), id(14)]);
        assert!(ops5[0].profit > 0);
        assert!(five.find_cycle(4).is_empty());
        let hops5 = vec![encode::PlanHop { venue: 0x03, direction: 0, n_accounts: 3, token_account_size: 0 }; 5];
        let body5 = encode::encode_route(ops5[0].amount_in, 0, &hops5, &encode::RouteMeta::new(1, 0)).unwrap();
        assert_eq!(body5.len(), 122, "n = 5 is 122 B in the window");
        assert_eq!(body5[25], 5);
    }

    #[test]
    fn four_hop_skips_a_shorter_cycle_and_respects_min_profit() {
        let base = id(1);
        let mut triangle = Engine::new(base, 100_000, 0);
        triangle.add_pool(cp_pool(10, 1, 2, 1_000_000, 2_000_000));
        triangle.add_pool(cp_pool(20, 2, 3, 1_000_000, 2_000_000));
        triangle.add_pool(cp_pool(30, 3, 1, 1_000_000, 2_000_000));
        assert!(!triangle.find_three_hop().is_empty());
        assert!(triangle.find_cycle(4).is_empty(), "a triangle is not a 4-leg route");

        let mut rich = doubling_cycle(4);
        rich.min_profit = u64::MAX;
        assert!(rich.find_cycle(4).is_empty());
    }

    #[test]
    fn best_opportunity_is_first() {
        let base = id(1);
        let mut e = Engine::new(base, 1_000_000, 0);
        // Pair T=2: moderate divergence.
        e.add_pool(cp_pool(10, 1, 2, 1_000_000, 1_200_000));
        e.add_pool(cp_pool(11, 1, 2, 1_200_000, 1_000_000));
        // Pair T=3: large divergence (larger profit).
        e.add_pool(cp_pool(20, 1, 3, 1_000_000, 3_000_000));
        e.add_pool(cp_pool(21, 1, 3, 3_000_000, 1_000_000));
        let ops = e.find_two_hop();
        assert!(ops.len() >= 2);
        assert_eq!(ops[0].token, id(3), "the larger divergence should come first");
        assert!(ops[0].profit >= ops[1].profit);
    }
}
