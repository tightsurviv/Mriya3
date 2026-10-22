//! Data input: pool state and pre-execution transactions (private orderflow relayer).
//!
//! Transport is separated from decoding by [`AccountSource`]. Thanks to this, the core
//! (decoding bytes → [`engine::Pool`]) is pure and testable on real mainnet accounts,
//! while the actual fetching (RPC `getMultipleAccounts`, later Geyser/shred/Preprocessed)
//! comes in as an implementation of this trait.
//!
//! Transport note: when the network has a proxy or an antivirus intercepting TLS, the RPC client must
//! be based on the system certificate store (native-tls), not the built-in rustls roots —
//! otherwise the HTTPS connection will fail.

pub mod accounts;
pub mod live;
pub mod pending;
pub mod relayer;
pub mod route;
pub mod run;
pub mod submit;
pub mod tpu;
pub mod transport;
pub mod wiring;

use pools::{parse_token_amount, pump_amm, Pubkey};

/// Source of raw account data (by key).
pub trait AccountSource {
    fn get_account(&self, key: &Pubkey) -> Option<&[u8]>;
}

/// Builds an [`engine::Pool`] (constant-product) from a Pump AMM pool: decodes the pool account, fetches
/// the balances of both vaults and maps them to reserves. `None` if any account is missing or they are inconsistent.
pub fn load_pump_pool(
    src: &impl AccountSource,
    pool_pubkey: &Pubkey,
    fee_bps: u32,
) -> Option<engine::Pool> {
    let pool_data = src.get_account(pool_pubkey)?;
    let p = pump_amm::decode(pool_data)?;
    let base_reserve = parse_token_amount(src.get_account(&p.base_vault)?)?;
    let quote_reserve = parse_token_amount(src.get_account(&p.quote_vault)?)?;
    Some(engine::Pool {
        id: *pool_pubkey,
        token_a: p.base_mint,
        token_b: p.quote_mint,
        kind: engine::Kind::Cp { res_a: base_reserve, res_b: quote_reserve, fee_bps },
    })
}

/// Builds a DLMM [`engine::Pool`] from an LbPair account + the active BinArray account.
///
/// `active_array_data` must be the BinArray containing the pair's `active_id` (index =
/// `bin_array_index(active_id)`). Bin window = 70 bins of the active array; neighbouring arrays
/// (wider window) will be added once the transport delivers several BinArrays at once.
pub fn dlmm_pool_from_parts(
    pool_pubkey: Pubkey,
    lb_pair_data: &[u8],
    active_array_data: &[u8],
) -> Option<engine::Pool> {
    let lb = pools::dlmm::decode_lb_pair(lb_pair_data)?;
    let arr_idx = pools::dlmm::bin_array_index_of(active_array_data)?;
    if arr_idx != pools::dlmm::bin_array_index(lb.active_id) {
        return None; // this is not the active BinArray
    }
    let bins = pools::dlmm::bins_of_array(active_array_data)?;
    let active_in_array = (lb.active_id as i64 - arr_idx * pools::dlmm::BINS_PER_ARRAY) as usize;
    Some(engine::Pool {
        id: pool_pubkey,
        token_a: lb.token_x_mint,
        token_b: lb.token_y_mint,
        // Base fee from the pool parameters (without the variable fee — that comes later).
        kind: engine::Kind::Dlmm { bins: bins.to_vec(), active: active_in_array, fee_bps: lb.base_fee_bps() },
    })
}

/// Builds a CLMM [`engine::Pool`] (Raydium, Pancake, Byreal) from a PoolState account — single-range pricing.
pub fn clmm_pool_from_account(
    pool_pubkey: Pubkey,
    pool_data: &[u8],
    fee_bps: u32,
) -> Option<engine::Pool> {
    let p = pools::clmm::decode(pool_data)?;
    Some(engine::Pool {
        id: pool_pubkey,
        token_a: p.token_mint_0,
        token_b: p.token_mint_1,
        kind: engine::Kind::Clmm {
            sqrt_price_x64: p.sqrt_price_x64,
            liquidity: p.liquidity,
            fee_bps,
            current_tick: p.tick_current,
            ticks: Vec::new(),
        },
    })
}

/// As above, but with the active TickArray account — pricing **with tick crossing**.
pub fn clmm_pool_with_ticks(
    pool_pubkey: Pubkey,
    pool_data: &[u8],
    tick_array_data: &[u8],
    fee_bps: u32,
) -> Option<engine::Pool> {
    let p = pools::clmm::decode(pool_data)?;
    let (_start, init) = pools::clmm::decode_tick_array(tick_array_data)?;
    let ticks: Vec<(i32, i128)> = init.iter().map(|t| (t.tick, t.liquidity_net)).collect();
    Some(engine::Pool {
        id: pool_pubkey,
        token_a: p.token_mint_0,
        token_b: p.token_mint_1,
        kind: engine::Kind::Clmm {
            sqrt_price_x64: p.sqrt_price_x64,
            liquidity: p.liquidity,
            fee_bps,
            current_tick: p.tick_current,
            ticks,
        },
    })
}

/// Builds an [`engine::Pool`] from an Orca Whirlpool account (CLMM model, priced like Raydium CLMM).
pub fn whirlpool_pool_from_account(pool_pubkey: Pubkey, pool_data: &[u8]) -> Option<engine::Pool> {
    let w = pools::whirlpool::decode(pool_data)?;
    Some(engine::Pool {
        id: pool_pubkey,
        token_a: w.token_mint_a,
        token_b: w.token_mint_b,
        kind: engine::Kind::Clmm {
            sqrt_price_x64: w.sqrt_price_x64,
            liquidity: w.liquidity,
            fee_bps: w.fee_bps(),
            current_tick: w.tick_current,
            ticks: Vec::new(),
        },
    })
}

/// Simple in-memory source (for tests and to be swapped for the real transport).
#[derive(Default)]
pub struct MapSource {
    map: std::collections::HashMap<Pubkey, Vec<u8>>,
}

impl MapSource {
    pub fn new() -> Self {
        Self { map: std::collections::HashMap::new() }
    }
    pub fn insert(&mut self, key: Pubkey, data: Vec<u8>) {
        self.map.insert(key, data);
    }
}

impl AccountSource for MapSource {
    fn get_account(&self, key: &Pubkey) -> Option<&[u8]> {
        self.map.get(key).map(|v| v.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Pump AMM pool account (JEBqxuDB…) from mainnet.
    const POOL_HEX: &str = "f19a6d0411b16dbcff0000fef83cee47c9b6733335c3ee9da78a707f3a8a511741a942061cea1e36b1a80d0d1fe32881fbd3dea81322c065e5eda2e2086644fec2565a0b1a312790ed053f069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001781c9d0ab5de0ea1e4b3b01ffeb24c28dccf9148586b6e2fe658646379c662cd94957e57ff09d3cc80e94fdfbd6777df13b34996d12844658d73a7cb318407c7818212cd53a985e7d5753b6de574ca5e4a3fdea8132639eee34f8a5d7c5cabff6400000000000000";
    // Real base vault account (B11Z2HEop…), 165 B, balance 26,748,411.
    const BASE_VAULT_HEX: &str = "0d1fe32881fbd3dea81322c065e5eda2e2086644fec2565a0b1a312790ed053ffff77d6689198ff0eb64d922de4d2c12a0b042c6a4bd3b6ff84f03fb023ae91cfb25980100000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    const POOL_ID: Pubkey = [255, 247, 125, 102, 137, 25, 143, 240, 235, 100, 217, 34, 222, 77, 44, 18, 160, 176, 66, 198, 164, 189, 59, 111, 248, 79, 3, 251, 2, 58, 233, 28];
    const BASE_VAULT: Pubkey = [148, 149, 126, 87, 255, 9, 211, 204, 128, 233, 79, 223, 189, 103, 119, 223, 19, 179, 73, 150, 209, 40, 68, 101, 141, 115, 167, 203, 49, 132, 7, 199];
    const QUOTE_VAULT: Pubkey = [129, 130, 18, 205, 83, 169, 133, 231, 213, 117, 59, 109, 229, 116, 202, 94, 74, 63, 222, 168, 19, 38, 57, 238, 227, 79, 138, 93, 124, 92, 171, 255];

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn token_account(amount: u64) -> Vec<u8> {
        let mut a = vec![0u8; 165];
        a[64..72].copy_from_slice(&amount.to_le_bytes());
        a
    }

    #[test]
    fn parses_real_vault_amount() {
        // Decoding a real SPL account from mainnet.
        assert_eq!(parse_token_amount(&hex(BASE_VAULT_HEX)), Some(26_748_411));
    }

    #[test]
    fn builds_engine_pool_from_real_pool_account() {
        // The pool account is real; we set the vault balances (vaults are separate accounts).
        let mut src = MapSource::new();
        src.insert(POOL_ID, hex(POOL_HEX));
        src.insert(BASE_VAULT, token_account(5_000_000));
        src.insert(QUOTE_VAULT, token_account(3_000_000));

        let pool = load_pump_pool(&src, &POOL_ID, 25).expect("should build the pool");
        assert_eq!(pool.id, POOL_ID);
        assert_eq!(pool.token_b, pools::WSOL); // quote = WSOL
        match &pool.kind {
            engine::Kind::Cp { res_a, res_b, fee_bps } => {
                assert_eq!(*res_a, 5_000_000); // base
                assert_eq!(*res_b, 3_000_000); // WSOL
                assert_eq!(*fee_bps, 25);
            }
            _ => unreachable!("Pump AMM → Cp"),
        }
    }

    #[test]
    fn builds_dlmm_pool_from_real_lb_pair() {
        let lb = hex(include_str!("testdata/lbpair_dlmm.hex"));
        let active_bin = hex(include_str!("testdata/dlmm_active_bin.hex"));
        // LbPair has active_id −240 → active bin at position 40 in array −4. We put a
        // real bin (bin data from mainnet) there as the active bin.
        let mut arr = vec![0u8; 56 + 70 * 144];
        arr[8..16].copy_from_slice(&(-4i64).to_le_bytes());
        let off = 56 + 40 * 144;
        arr[off..off + 144].copy_from_slice(&active_bin);

        let pool = dlmm_pool_from_parts([9u8; 32], &lb, &arr).expect("DLMM pool");
        assert_eq!(pool.token_b, pools::WSOL); // Y = WSOL
        match &pool.kind {
            engine::Kind::Dlmm { active, bins, fee_bps } => {
                assert_eq!(*active, 40);
                assert_eq!(bins.len(), 70);
                assert_eq!(bins[40].reserve_x, 8_068_787_434);
                assert_eq!(*fee_bps, 80); // base fee from the pool parameters
            }
            _ => unreachable!(),
        }
        // Buying X with Y (Y→X, price rises) should give > 0 (the active bin has X in stock).
        let x_out = pool.quote(100_000_000, false);
        assert!(x_out > 0, "quote Y→X should be positive, was {x_out}");
    }

    #[test]
    fn builds_clmm_pool_from_real_account() {
        let data = hex(include_str!("testdata/clmm_ray.hex"));
        let pool = clmm_pool_from_account([5u8; 32], &data, 25).expect("CLMM pool");
        match &pool.kind {
            engine::Kind::Clmm { sqrt_price_x64, liquidity, fee_bps, .. } => {
                assert_eq!(*sqrt_price_x64, 1_930_420_496_479_574_184);
                assert_eq!(*liquidity, 213_072_157_226);
                assert_eq!(*fee_bps, 25);
            }
            _ => unreachable!(),
        }
        // A token0→token1 swap (zero_for_one) should give > 0 with non-zero liquidity.
        let out = pool.quote(1_000_000, true);
        assert!(out > 0, "quote CLMM should be positive, was {out}");
    }

    #[test]
    fn builds_whirlpool_pool_from_real_account() {
        let data = hex(include_str!("testdata/whirlpool.hex"));
        let pool = whirlpool_pool_from_account([6u8; 32], &data).expect("Whirlpool pool");
        match &pool.kind {
            engine::Kind::Clmm { sqrt_price_x64, liquidity, fee_bps, .. } => {
                assert_eq!(*sqrt_price_x64, 325_299_095_337_862_195);
                assert_eq!(*liquidity, 881_874_867_361_078_231);
                assert_eq!(*fee_bps, 1); // fee_rate 100 → 1 bps
            }
            _ => unreachable!(),
        }
        assert!(pool.quote(1_000_000, true) > 0);
    }

    #[test]
    fn clmm_with_ticks_uses_crossing() {
        // Same pool as the CLMM fixture; this pool's active TickArray → pricing with crossing.
        let pool_data = hex(include_str!("testdata/clmm_ray.hex"));
        let tick_array = hex(include_str!("testdata/clmm_tickarray.hex"));
        let pool = clmm_pool_with_ticks([8u8; 32], &pool_data, &tick_array, 25).expect("CLMM+ticks");
        match &pool.kind {
            engine::Kind::Clmm { ticks, current_tick, .. } => {
                assert_eq!(ticks.len(), 9, "9 initialized ticks from the active array");
                assert_eq!(*current_tick, -45_146);
            }
            _ => unreachable!(),
        }
        // Pricing works (a large swap that may cross a tick) and is positive.
        assert!(pool.quote(1_000_000_000, true) > 0);
    }

    #[test]
    fn end_to_end_feed_into_engine_finds_arb() {
        // A real pool + a second, synthetic one on the same pair at a different price → opportunity.
        let mut src = MapSource::new();
        src.insert(POOL_ID, hex(POOL_HEX));
        src.insert(BASE_VAULT, token_account(1_000_000));
        src.insert(QUOTE_VAULT, token_account(2_000_000)); // here WSOL is "expensive" in base

        let p1 = load_pump_pool(&src, &POOL_ID, 25).unwrap();
        // base = p1.token_a (mint base), quote = WSOL. Arbitrage base = WSOL.
        let base = p1.token_b;
        let other = p1.token_a;

        // second pool: same pair, opposite divergence
        let p2 = engine::Pool {
            id: [7u8; 32],
            token_a: other,
            token_b: base,
            kind: engine::Kind::Cp { res_a: 2_000_000, res_b: 1_000_000, fee_bps: 25 },
        };

        let mut e = engine::Engine::new(base, 500_000, 0);
        e.add_pool(p1);
        e.add_pool(p2);
        let ops = e.find_two_hop();
        assert!(!ops.is_empty(), "feed→engine should find an opportunity");
        assert!(ops[0].profit > 0);
    }
}
