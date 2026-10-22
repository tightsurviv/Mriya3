//! Decoding Raydium CLMM (concentrated liquidity). Layout verified on a real account
//! (program `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`, 1544 B account).
//!
//! PoolState offsets: bump @8, amm_config @9, owner @41, token_mint_0 @73, token_mint_1 @105,
//! token_vault_0 @137, token_vault_1 @169, observation @201, mint_decimals_0 @233,
//! mint_decimals_1 @234, tick_spacing u16 @235, liquidity u128 @237, sqrt_price_x64 u128 @253,
//! tick_current i32 @269. Pancake and Byreal have the same layout: 1544 B accounts, and the mints and
//! vaults at these offsets match the legs of the window.
//!
//! `sqrt_price_x64 / 2^64 = √(raw price: token1 per token0)`. Match confirmed: the price from
//! sqrt_price equals `1.0001^tick_current`.

use crate::Pubkey;

pub const PROGRAM_ID_B58: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const MIN_LEN: usize = 273; // up to the end of tick_current

const OFF_MINT0: usize = 73;
const OFF_MINT1: usize = 105;
const OFF_VAULT0: usize = 137;
const OFF_VAULT1: usize = 169;
const OFF_DEC0: usize = 233;
const OFF_DEC1: usize = 234;
const OFF_TICK_SPACING: usize = 235;
const OFF_LIQUIDITY: usize = 237;
const OFF_SQRT_PRICE: usize = 253;
const OFF_TICK_CURRENT: usize = 269;

/// CLMM pool state (fields relevant for pricing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClmmPool {
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
}

fn pubkey(d: &[u8], o: usize) -> Pubkey {
    let mut k = [0u8; 32];
    k.copy_from_slice(&d[o..o + 32]);
    k
}
fn u128le(d: &[u8], o: usize) -> u128 {
    u128::from_le_bytes(d[o..o + 16].try_into().unwrap())
}

/// Decodes a Raydium CLMM PoolState account (or Pancake CLMM — same layout).
pub fn decode(data: &[u8]) -> Option<ClmmPool> {
    if data.len() < MIN_LEN {
        return None;
    }
    Some(ClmmPool {
        token_mint_0: pubkey(data, OFF_MINT0),
        token_mint_1: pubkey(data, OFF_MINT1),
        token_vault_0: pubkey(data, OFF_VAULT0),
        token_vault_1: pubkey(data, OFF_VAULT1),
        mint_decimals_0: data[OFF_DEC0],
        mint_decimals_1: data[OFF_DEC1],
        tick_spacing: u16::from_le_bytes(data[OFF_TICK_SPACING..OFF_TICK_SPACING + 2].try_into().ok()?),
        liquidity: u128le(data, OFF_LIQUIDITY),
        sqrt_price_x64: u128le(data, OFF_SQRT_PRICE),
        tick_current: i32::from_le_bytes(data[OFF_TICK_CURRENT..OFF_TICK_CURRENT + 4].try_into().ok()?),
    })
}

impl ClmmPool {
    /// √(raw price) as f64 (for the [`sim::clmm`] simulator).
    pub fn sqrt_price_f64(&self) -> f64 {
        self.sqrt_price_x64 as f64 / 2f64.powi(64)
    }
}

// --- TickArray (Raydium CLMM): 10240 B account, 60 ticks of 168 B each ---
const TICK_ARRAY_POOL_OFF: usize = 8;
const TICK_ARRAY_START_OFF: usize = 40;
const TICKS_OFF: usize = 44;
const TICK_SIZE: usize = 168;
pub const TICKS_PER_ARRAY: usize = 60;

/// Initialized tick: the tick index and its `liquidity_net` (change in L on crossing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitTick {
    pub tick: i32,
    pub liquidity_net: i128,
}

/// Address of the pool that the TickArray belongs to.
pub fn tick_array_pool(data: &[u8]) -> Option<Pubkey> {
    if data.len() < TICKS_OFF {
        return None;
    }
    Some(pubkey(data, TICK_ARRAY_POOL_OFF))
}

/// Decodes a TickArray: (start_tick_index, initialized ticks ascending by index).
pub fn decode_tick_array(data: &[u8]) -> Option<(i32, Vec<InitTick>)> {
    if data.len() < TICKS_OFF + TICKS_PER_ARRAY * TICK_SIZE {
        return None;
    }
    let start = i32::from_le_bytes(data[TICK_ARRAY_START_OFF..TICK_ARRAY_START_OFF + 4].try_into().ok()?);
    let mut ticks = Vec::new();
    for i in 0..TICKS_PER_ARRAY {
        let off = TICKS_OFF + i * TICK_SIZE;
        let tick = i32::from_le_bytes(data[off..off + 4].try_into().ok()?);
        let liquidity_net = i128::from_le_bytes(data[off + 4..off + 20].try_into().ok()?);
        let liquidity_gross = u128::from_le_bytes(data[off + 20..off + 36].try_into().ok()?);
        if liquidity_gross != 0 {
            ticks.push(InitTick { tick, liquidity_net });
        }
    }
    Some((start, ticks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn decodes_real_raydium_clmm() {
        let data = hex(include_str!("testdata/clmm_ray.hex"));
        let p = decode(&data).expect("CLMM decode");
        // mint1 = USDC — confirms the offsets are aligned.
        assert_eq!(
            p.token_mint_1,
            [198, 250, 122, 243, 190, 219, 173, 58, 61, 101, 243, 106, 171, 201, 116, 49, 177, 187, 228, 194, 210, 246, 224, 228, 124, 166, 2, 3, 69, 47, 93, 97]
        );
        assert_eq!(p.mint_decimals_0, 6);
        assert_eq!(p.mint_decimals_1, 6);
        assert_eq!(p.tick_spacing, 60);
        assert_eq!(p.liquidity, 213_072_157_226);
        assert_eq!(p.sqrt_price_x64, 1_930_420_496_479_574_184); // from the fixture's bytes (live pool)
        assert_eq!(p.tick_current, -45_146);
    }

    #[test]
    fn price_matches_tick() {
        let data = hex(include_str!("testdata/clmm_ray.hex"));
        let p = decode(&data).unwrap();
        let price_from_sqrt = p.sqrt_price_f64() * p.sqrt_price_f64();
        let price_from_tick = 1.0001f64.powi(p.tick_current);
        // Both methods give the same raw price (±0.5%).
        assert!((price_from_sqrt - price_from_tick).abs() / price_from_tick < 0.005);
    }

    #[test]
    fn decodes_real_tick_array() {
        let data = hex(include_str!("testdata/clmm_tickarray.hex"));
        assert_eq!(data.len(), 10240);
        let (start, ticks) = decode_tick_array(&data).unwrap();
        assert_eq!(start, -46_800);
        assert_eq!(ticks.len(), 9);
        assert_eq!(ticks[0], InitTick { tick: -46_080, liquidity_net: 187_687_145_256 });
        // The sum of liquidity_net over all ticks on both sides of the range gives 0 (L balance).
        // Here we check for the presence of a balancing pair: +25.7e9 at -44700, −25.7e9 at -43800.
        assert!(ticks.iter().any(|t| t.tick == -44_700 && t.liquidity_net == 25_739_204_900));
        assert!(ticks.iter().any(|t| t.tick == -43_800 && t.liquidity_net == -25_739_204_900));
    }
}
