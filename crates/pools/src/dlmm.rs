//! Decoding Meteora DLMM: pair account (LbPair) + bin accounts (BinArray).
//!
//! Layout determined and verified on real mainnet accounts (program
//! `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`):
//! - **LbPair** (904 B): active_id i32 @76, bin_step u16 @80, token_x_mint @88, token_y_mint @120,
//!   reserve_x (vault) @152, reserve_y (vault) @184.
//! - **BinArray** (10136 B): index i64 @8, lb_pair @24, bins @56 — 70 bins of 144 B each;
//!   in a bin: amount_x u64 @0, amount_y u64 @8, price u128 (Q64.64, Y per X) @16.
//!
//! `price` is already stored in the bin (no need to compute `(1+bin_step)^id`), which was verified:
//! the active bin id −238 had price/2^64 = 0.15010438 = 1.008^−238.

use crate::Pubkey;
use sim::dlmm::Bin;

pub const PROGRAM_ID_B58: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
pub const LB_PAIR_LEN: usize = 904;
pub const BINS_PER_ARRAY: i64 = 70;

const OFF_BASE_FACTOR: usize = 8;
const OFF_VARIABLE_FEE_CONTROL: usize = 16;
const OFF_MAX_VOL_ACCUM: usize = 20;
const OFF_BASE_FEE_POWER: usize = 34;
const OFF_ACTIVE_ID: usize = 76;
const OFF_BIN_STEP: usize = 80;
const OFF_X_MINT: usize = 88;
const OFF_Y_MINT: usize = 120;
const OFF_RES_X: usize = 152;
const OFF_RES_Y: usize = 184;

const BIN_ARRAY_INDEX_OFF: usize = 8;
const BIN_ARRAY_LBPAIR_OFF: usize = 24;
const BINS_OFFSET: usize = 56;
const BIN_SIZE: usize = 144;

/// DLMM pair metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LbPair {
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub active_id: i32,
    pub bin_step: u16,
    pub base_factor: u16,
    pub base_fee_power_factor: u8,
    pub variable_fee_control: u32,
    pub max_volatility_accumulator: u32,
}

impl LbPair {
    /// Meteora fee precision: 1e9 = 100%.
    const FEE_PRECISION_1E9: u128 = 1_000_000_000;
    /// Upper limit of the total fee (10% = 1e8 in 1e9 units).
    const MAX_FEE_1E9: u128 = 100_000_000;

    /// Base fee (constant) in 1e9 units:`base_factor·bin_step·10·10^power`.
    fn base_fee_1e9(&self) -> u128 {
        self.base_factor as u128
            * self.bin_step as u128
            * 10
            * 10u128.pow(self.base_fee_power_factor as u32)
    }

    /// Base fee (constant) in basis points (1e9 units → bps via /1e5).
    /// Does not include the variable volatility fee — see [`LbPair::total_fee_bps`].
    pub fn base_fee_bps(&self) -> u32 {
        (self.base_fee_1e9() / 100_000) as u32
    }

    /// Variable (volatility) fee in 1e9 units, for accumulator `v_a` (Meteora model):
    /// `ceil(variable_fee_control · (v_a·bin_step)² / 1e11)`. 0 when `variable_fee_control == 0`.
    /// `v_a` is per-swap state (volatility accumulator) — supplied by the data layer from the pool state.
    pub fn variable_fee_1e9(&self, v_a: u32) -> u128 {
        if self.variable_fee_control == 0 {
            return 0;
        }
        let square = (v_a as u128 * self.bin_step as u128).pow(2);
        let v_fee = self.variable_fee_control as u128 * square;
        (v_fee + 99_999_999_999) / 100_000_000_000
    }

    /// **Total** fee (base + variable) in bps for accumulator `v_a`, capped at 10%.
    /// `total_fee_bps(0) == base_fee_bps()`.
    pub fn total_fee_bps(&self, v_a: u32) -> u32 {
        let total = (self.base_fee_1e9() + self.variable_fee_1e9(v_a)).min(Self::MAX_FEE_1E9);
        (total / 100_000) as u32
    }

    /// Whether the fee is expressible in whole bps (debug): 1e9 precision allows fractions of bps.
    #[doc(hidden)]
    pub fn fee_precision_1e9() -> u128 {
        Self::FEE_PRECISION_1E9
    }
}

fn pubkey(data: &[u8], off: usize) -> Pubkey {
    let mut k = [0u8; 32];
    k.copy_from_slice(&data[off..off + 32]);
    k
}

/// Decodes an LbPair account.
pub fn decode_lb_pair(data: &[u8]) -> Option<LbPair> {
    if data.len() < LB_PAIR_LEN {
        return None;
    }
    Some(LbPair {
        token_x_mint: pubkey(data, OFF_X_MINT),
        token_y_mint: pubkey(data, OFF_Y_MINT),
        reserve_x: pubkey(data, OFF_RES_X),
        reserve_y: pubkey(data, OFF_RES_Y),
        active_id: i32::from_le_bytes(data[OFF_ACTIVE_ID..OFF_ACTIVE_ID + 4].try_into().ok()?),
        bin_step: u16::from_le_bytes(data[OFF_BIN_STEP..OFF_BIN_STEP + 2].try_into().ok()?),
        base_factor: u16::from_le_bytes(data[OFF_BASE_FACTOR..OFF_BASE_FACTOR + 2].try_into().ok()?),
        base_fee_power_factor: data[OFF_BASE_FEE_POWER],
        variable_fee_control: u32::from_le_bytes(data[OFF_VARIABLE_FEE_CONTROL..OFF_VARIABLE_FEE_CONTROL + 4].try_into().ok()?),
        max_volatility_accumulator: u32::from_le_bytes(data[OFF_MAX_VOL_ACCUM..OFF_MAX_VOL_ACCUM + 4].try_into().ok()?),
    })
}

/// Index of the BinArray containing the given bin (division rounds down).
pub fn bin_array_index(bin_id: i32) -> i64 {
    (bin_id as i64).div_euclid(BINS_PER_ARRAY)
}

/// Global bin id for (array index, position within the array).
pub fn global_bin_id(array_index: i64, i_in_array: usize) -> i32 {
    (array_index * BINS_PER_ARRAY + i_in_array as i64) as i32
}

/// Reads the BinArray index from the account header.
pub fn bin_array_index_of(data: &[u8]) -> Option<i64> {
    if data.len() < BIN_ARRAY_LBPAIR_OFF {
        return None;
    }
    Some(i64::from_le_bytes(data[BIN_ARRAY_INDEX_OFF..BIN_ARRAY_INDEX_OFF + 8].try_into().ok()?))
}

/// Address of the LbPair that the BinArray belongs to.
pub fn bin_array_lb_pair(data: &[u8]) -> Option<Pubkey> {
    if data.len() < BINS_OFFSET {
        return None;
    }
    Some(pubkey(data, BIN_ARRAY_LBPAIR_OFF))
}

/// Reads bin `i` (0..70) from a BinArray account as a [`sim::dlmm::Bin`].
pub fn read_bin(data: &[u8], i_in_array: usize) -> Option<Bin> {
    if i_in_array >= BINS_PER_ARRAY as usize {
        return None;
    }
    let off = BINS_OFFSET + i_in_array * BIN_SIZE;
    if data.len() < off + 32 {
        return None;
    }
    let amount_x = u64::from_le_bytes(data[off..off + 8].try_into().ok()?);
    let amount_y = u64::from_le_bytes(data[off + 8..off + 16].try_into().ok()?);
    let price_x64 = u128::from_le_bytes(data[off + 16..off + 32].try_into().ok()?);
    Some(Bin { price_x64, reserve_x: amount_x, reserve_y: amount_y })
}

/// All 70 bins from a single BinArray (ascending by id = ascending by price).
pub fn bins_of_array(data: &[u8]) -> Option<[Bin; 70]> {
    let mut out = [Bin { price_x64: 0, reserve_x: 0, reserve_y: 0 }; 70];
    for i in 0..70 {
        out[i] = read_bin(data, i)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn decodes_real_lb_pair() {
        let data = hex(include_str!("testdata/lbpair_dlmm.hex"));
        assert_eq!(data.len(), LB_PAIR_LEN);
        let lb = decode_lb_pair(&data).unwrap();
        assert_eq!(lb.active_id, -240); // from the fixture's bytes (live pool; active_id drifts)
        assert_eq!(lb.bin_step, 80);
        assert_eq!(lb.base_factor, 10_000);
        assert_eq!(lb.base_fee_bps(), 80); // 10000·80·10/1e9 = 0.8% = 80 bps
        assert_eq!(lb.token_y_mint, crate::WSOL); // Y = WSOL
    }

    #[test]
    fn bin_array_math() {
        assert_eq!(bin_array_index(-238), -4);
        assert_eq!(bin_array_index(0), 0);
        assert_eq!(bin_array_index(69), 0);
        assert_eq!(bin_array_index(70), 1);
        // active bin −238 sits at position 42 in array −4
        assert_eq!(global_bin_id(-4, 42), -238);
    }

    fn syn(bin_step: u16, base_factor: u16, power: u8, vfc: u32) -> LbPair {
        LbPair {
            token_x_mint: [0u8; 32],
            token_y_mint: [0u8; 32],
            reserve_x: [0u8; 32],
            reserve_y: [0u8; 32],
            active_id: 0,
            bin_step,
            base_factor,
            base_fee_power_factor: power,
            variable_fee_control: vfc,
            max_volatility_accumulator: 0,
        }
    }

    #[test]
    fn variable_fee_model() {
        // Anchor on a real account: v_a=0 → total fee == base (80 bps).
        let data = hex(include_str!("testdata/lbpair_dlmm.hex"));
        let lb = decode_lb_pair(&data).unwrap();
        assert_eq!(lb.total_fee_bps(0), lb.base_fee_bps());
        assert_eq!(lb.total_fee_bps(0), 80);

        // Meteora formula: ceil(C·(v_a·bin_step)²/1e11). bin_step=10, C=5000, v_a=1000 → 5 (1e9).
        let s = syn(10, 10_000, 0, 5000);
        assert_eq!(s.variable_fee_1e9(1000), 5);
        assert_eq!(s.variable_fee_1e9(0), 0);
        // variable_fee_control = 0 → no variable fee regardless of v_a.
        assert_eq!(syn(10, 10_000, 0, 0).variable_fee_1e9(100_000), 0);
        // 10% limit = 1000 bps at large v_a.
        assert_eq!(syn(100, 10_000, 0, 100_000).total_fee_bps(100_000), 1000);
    }

    #[test]
    fn reads_real_active_bin() {
        // A real active bin (144 B) placed at position 42 in a synthetic array.
        let bin = hex(include_str!("testdata/dlmm_active_bin.hex"));
        assert_eq!(bin.len(), 144);
        let mut arr = vec![0u8; 56 + 70 * 144];
        let off = 56 + 42 * 144;
        arr[off..off + 144].copy_from_slice(&bin);
        let b = read_bin(&arr, 42).unwrap();
        assert_eq!(b.reserve_x, 8_068_787_434);
        assert_eq!(b.reserve_y, 0);
        assert_eq!(b.price_x64, 2_768_937_037_119_196_846); // = 0.15010438 · 2^64
    }
}
