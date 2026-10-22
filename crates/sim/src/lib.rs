//! `sim` — AMM math for the arbitrage bot (swap quoting, route profit, size selection).
//!
//! Pure functions, no network and no I/O — the same math used by every aggregator
//! and DEX interface. Models:
//! - [`cp`]   — constant product (Raydium v4/CPMM, Pump AMM, Meteora DAMM),
//! - [`dlmm`] — Meteora bins (constant sum within a bin), integer arithmetic,
//! - [`clmm`] — concentrated liquidity (Uniswap v3 / Raydium CLMM / Whirlpool), f64 pricing,
//! - [`book`] — Manifest order book (walk down the bids and asks).
//!
//! As a helper, [`math`] provides `mul_div` with a 256-bit intermediate.

pub mod book;
pub mod clmm;
pub mod cp;
pub mod dlmm;
pub mod math;
pub mod opt;
pub mod u256;

pub use cp::{amount_out, optimal_two_pool, two_pool_out, two_pool_profit, CpPool, Opt};
pub use opt::argmax_unimodal;
