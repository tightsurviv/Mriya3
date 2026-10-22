//! Decoding Orca Whirlpool (v3-style CLMM). Layout verified on a real account
//! (program `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`, 653 B account).
//!
//! Offsets: tick_spacing u16 @41, fee_rate u16 @45 (units of 1e-6), liquidity u128 @49,
//! sqrt_price u128 @65 (Q64.64), tick_current_index i32 @81, token_mint_a @101,
//! token_vault_a @133, token_mint_b @181, token_vault_b @213.
//!
//! Model identical to Raydium CLMM (sqrt_price Q64.64), so pricing goes through [`sim::clmm`].
//! Match confirmed: `(sqrt_price/2^64)^2 == 1.0001^tick_current`.

use crate::Pubkey;

pub const PROGRAM_ID_B58: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const MIN_LEN: usize = 245;

const OFF_TICK_SPACING: usize = 41;
const OFF_FEE_RATE: usize = 45;
const OFF_LIQUIDITY: usize = 49;
const OFF_SQRT_PRICE: usize = 65;
const OFF_TICK_CURRENT: usize = 81;
const OFF_MINT_A: usize = 101;
const OFF_VAULT_A: usize = 133;
const OFF_MINT_B: usize = 181;
const OFF_VAULT_B: usize = 213;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Whirlpool {
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    pub tick_spacing: u16,
    /// Rate in units of 1e-6 (e.g. 3000 = 0.30%). `fee_bps()` converts to basis points.
    pub fee_rate: u16,
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

pub fn decode(data: &[u8]) -> Option<Whirlpool> {
    if data.len() < MIN_LEN {
        return None;
    }
    Some(Whirlpool {
        token_mint_a: pubkey(data, OFF_MINT_A),
        token_mint_b: pubkey(data, OFF_MINT_B),
        token_vault_a: pubkey(data, OFF_VAULT_A),
        token_vault_b: pubkey(data, OFF_VAULT_B),
        tick_spacing: u16::from_le_bytes(data[OFF_TICK_SPACING..OFF_TICK_SPACING + 2].try_into().ok()?),
        fee_rate: u16::from_le_bytes(data[OFF_FEE_RATE..OFF_FEE_RATE + 2].try_into().ok()?),
        liquidity: u128le(data, OFF_LIQUIDITY),
        sqrt_price_x64: u128le(data, OFF_SQRT_PRICE),
        tick_current: i32::from_le_bytes(data[OFF_TICK_CURRENT..OFF_TICK_CURRENT + 4].try_into().ok()?),
    })
}

impl Whirlpool {
    /// Rate in basis points (fee_rate/100; sub-bps rounded down).
    pub fn fee_bps(&self) -> u32 {
        self.fee_rate as u32 / 100
    }
    pub fn sqrt_price_f64(&self) -> f64 {
        self.sqrt_price_x64 as f64 / 2f64.powi(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn decodes_real_whirlpool() {
        let data = hex(include_str!("testdata/whirlpool.hex"));
        let w = decode(&data).expect("whirlpool decode");
        assert_eq!(
            w.token_mint_b,
            [198, 250, 122, 243, 190, 219, 173, 58, 61, 101, 243, 106, 171, 201, 116, 49, 177, 187, 228, 194, 210, 246, 224, 228, 124, 166, 2, 3, 69, 47, 93, 97]
        ); // USDC
        assert_eq!(w.tick_spacing, 1);
        assert_eq!(w.fee_rate, 100);
        assert_eq!(w.fee_bps(), 1);
        assert_eq!(w.liquidity, 881_874_867_361_078_231);
        assert_eq!(w.sqrt_price_x64, 325_299_095_337_862_195);
        assert_eq!(w.tick_current, -80_762);
    }

    #[test]
    fn price_matches_tick() {
        let w = decode(&hex(include_str!("testdata/whirlpool.hex"))).unwrap();
        let from_sqrt = w.sqrt_price_f64() * w.sqrt_price_f64();
        let from_tick = 1.0001f64.powi(w.tick_current);
        assert!((from_sqrt - from_tick).abs() / from_tick < 0.005);
    }
}
