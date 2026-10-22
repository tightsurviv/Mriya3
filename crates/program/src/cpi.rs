//! CPI instruction data for exchanges (swap) — **allocation-free** encoding into a buffer (`no_std` program,
//! "no_alloc"). Discriminator + arguments, `min_out ≤ 1` per leg (safety
//! comes from the final profit check, not leg slippage).
//!
//! **Discriminators** (Anchor `sha256("global:<name>")[..8]`) computed from names confirmed in the
//! disassembly of the reference bot — verified in a test via [`keys::sha256`]. **Argument layouts**
//! come from the inner CPIs of a window of real mainnet txs (2026-09-26). The second amount threshold is 0 except for Pump AMM and Pump.fun, where `min_out` is passed (1 in the window).

/// Venue codes (hop[0]) — as in the reference bot's program.
pub mod venue {
    /// Meteora DAMM v1. Same `swap` bytes as [`DAMM_V2`], different account list.
    pub const DAMM_V1: u8 = 0x00;
    pub const DLMM: u8 = 0x01;
    pub const RAYDIUM_CLMM: u8 = 0x02;
    pub const RAYDIUM_CPMM: u8 = 0x03;
    pub const WHIRLPOOL: u8 = 0x04;
    pub const PUMP_AMM: u8 = 0x05;
    pub const PANCAKE_CLMM: u8 = 0x07;
    pub const DAMM_V2: u8 = 0x08;
    /// Byreal CLMM. One 41 B template in the window.
    pub const BYREAL: u8 = 0x0a;
    /// Saros AMM. SPL token-swap fork, instruction tag `0x01`.
    pub const SAROS: u8 = 0x0d;
    /// Pump.fun bonding curve. Direction 0 = `buy_exact_quote_in_v2`, direction 1 = `sell_v2`.
    pub const PUMP_FUN: u8 = 0x0e;
    /// Meteora DBC. Same 24 B as DAMM `swap`. One failed leg in the window.
    pub const METEORA_DBC: u8 = 0x12;
    /// Raydium LaunchLab. Direction 0 = `buy_exact_in`, direction 1 = `sell_exact_in`.
    pub const LAUNCHPAD: u8 = 0x13;
    /// BSwp. Direction 0 only: `swap`, amount, u128 MAX limit, byte 0.
    pub const BSWP: u8 = 0x14;
    /// Orca token-swap v2. Same SPL layout as Saros.
    pub const ORCA_TOKEN_SWAP: u8 = 0x15;
    pub const MANIFEST: u8 = 0x10;
    /// MetaDAO Futarchy `spot_swap`.
    pub const FUTARCHY: u8 = 0x16;
    /// DFlow Aggregator `swap2` with a BisonFi step.
    pub const DFLOW: u8 = 0x17;
    /// DLMM `swap` (not `swap2`). Same exchange as [`DLMM`], older instruction.
    pub const DLMM_V1: u8 = 0x23;
    /// Raydium CLMM `swap` (not `swap_v2`).
    pub const RAYDIUM_CLMM_V1: u8 = 0x24;
    /// Orca Whirlpool `swap` (not `swap_v2`).
    pub const WHIRLPOOL_V1: u8 = 0x25;
    /// Raydium AMM v4. Instruction tag `0x10`, not an Anchor discriminator.
    pub const RAYDIUM_V4: u8 = 0x26;
    /// Pancake CLMM `swap` (not `swap_v2`).
    pub const PANCAKE_CLMM_V1: u8 = 0x27;
    /// GoonFi v2. The byte after tag `0x01` is the inverse of the route direction.
    pub const GOONFI: u8 = 0x1b;
    /// Third SPL token-swap fork (`SwaPpA9L…`). One failed leg.
    pub const SWAP_A9: u8 = 0x1d;
    /// AlphaQ. Direction 0 only, tag `0x0c 0x00`.
    pub const ALPHAQ: u8 = 0x21;
    /// Hylo. Two discriminators, one per direction. The account list is fixed.
    pub const HYLO: u8 = 0x29;
}

/// Anchor discriminators (`sha256("global:<name>")[..8]`).
pub mod disc {
    /// `swap2` — Meteora DLMM.
    pub const SWAP2: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
    /// `swap_v2` — Raydium CLMM / Orca Whirlpool.
    pub const SWAP_V2: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];
    /// `swap_base_input` — Raydium CPMM.
    pub const SWAP_BASE_INPUT: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
    /// `swap` — Meteora DAMM v2 (and others).
    pub const SWAP: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
    /// `buy` — Pump AMM, exact-out. This discriminator does not appear in the window of real txs.
    pub const BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
    /// `buy_exact_quote_in` — Pump AMM. 4,058/4,058 CPIs in the window.
    pub const BUY_EXACT_QUOTE_IN: [u8; 8] = [0xc6, 0x2e, 0x15, 0x52, 0xb4, 0xd9, 0xe8, 0x70];
    /// `sell` — Pump AMM.
    pub const SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
    /// `spot_swap` — MetaDAO Futarchy.
    pub const SPOT_SWAP: [u8; 8] = [0xa7, 0x61, 0x0c, 0xe7, 0xed, 0x4e, 0xa6, 0xfb];
    /// Byreal. 98/98 CPIs start with these bytes. This is not `sha256("global:swap")`.
    pub const BYREAL_SWAP: [u8; 8] = [0xe5, 0x2e, 0xd5, 0x84, 0x69, 0x28, 0x28, 0xe4];
    /// `buy_exact_quote_in_v2` — Pump.fun, direction 0. 22/22 CPIs with data.
    pub const BUY_EXACT_QUOTE_IN_V2: [u8; 8] = [0xc2, 0xab, 0x1c, 0x46, 0x68, 0x4d, 0x5b, 0x2f];
    /// `sell_v2` — Pump.fun, direction 1. 11/11 CPIs with data.
    pub const SELL_V2: [u8; 8] = [0x5d, 0xf6, 0x82, 0x3c, 0xe7, 0xe9, 0x40, 0xb2];
    /// `buy_exact_in` — Raydium LaunchLab, direction 0. 11/11.
    pub const BUY_EXACT_IN: [u8; 8] = [0xfa, 0xea, 0x0d, 0x7b, 0xd5, 0x9c, 0x13, 0xec];
    /// `sell_exact_in` — Raydium LaunchLab, direction 1. 13/13.
    pub const SELL_EXACT_IN: [u8; 8] = [0x95, 0x27, 0xde, 0x9b, 0xd3, 0x7c, 0x98, 0x1a];
    /// Hylo, direction 0. 2/2 CPIs. The Anchor name did not turn up in the checked set.
    pub const HYLO_0: [u8; 8] = [0x33, 0x62, 0xdf, 0xca, 0x0d, 0x61, 0xb7, 0x02];
    /// Hylo, direction 1. 5/5 CPIs.
    pub const HYLO_1: [u8; 8] = [0xdf, 0x26, 0xfc, 0xe2, 0x0c, 0xfb, 0xc2, 0xd7];
}

/// Orca Whirlpool sqrt-price bounds (Q64.64). `swap` v1 sets them as no limit.
/// `swap_v2` in the window sets 0.
pub const WHIRLPOOL_MIN_SQRT_PRICE: u128 = 4295048016;
pub const WHIRLPOOL_MAX_SQRT_PRICE: u128 = 79226673515401279992447579055;

/// Raydium / Pancake CLMM `swap` v1: MIN+1 and MAX−1. `swap_v2` in the window sets 0.
pub const RAYDIUM_SQRT_MIN_PLUS_ONE: u128 = 4295048017;
pub const RAYDIUM_SQRT_MAX_MINUS_ONE: u128 = 79226673521066979257578248090;

/// Leg direction. Pump: 0 = sell (base→quote), 1 = `buy_exact_quote_in`.
/// Whirlpool and CLMM v1: the direction byte is copied as `a_to_b` / zero_for_one, so 1 = a→b.
pub const DIR_A_TO_B: u8 = 0;

/// Manifest `SwapV2`. Tag 0x0d, 3,261/3,261 CPIs in the window.
const MANIFEST_SWAP_V2: u8 = 0x0d;

fn put(buf: &mut [u8], off: usize, bytes: &[u8]) -> Option<usize> {
    let end = off + bytes.len();
    if end > buf.len() {
        return None;
    }
    buf[off..end].copy_from_slice(bytes);
    Some(end)
}

/// Encodes the swap instruction data for the given venue into `buf`, returns the length.
///
/// The bytes come from the window's CPIs (stack height 2). `min_out` is written for Pump AMM and Pump.fun
/// (in the window the second `u64` is 1). In the other measured instructions the second `u64` is 0
/// or absent, so the argument is skipped — the caller passes 1 anyway.
///
/// - **CPMM** (0x03): `swap_base_input`, 24 B, threshold 0 (1,545/1,545).
/// - **Raydium CLMM** (0x02) and **Pancake** (0x07) `swap_v2`: 49 B, sqrt 0, `is_base_input` 1,
///   then `u64` 0 (1,188/1,188 and 4/4). The direction is in the accounts.
/// - **DLMM** (0x01) `swap2`: 28 B, threshold 0, empty slices (6,746/6,746).
/// - **DLMM v1** (0x23) `swap`: 24 B, threshold 0 (1,974/1,974).
/// - **CLMM v1** (0x24) and **Pancake v1** (0x27) `swap`: 41 B. Direction 1 → sqrt MIN+1,
///   direction 0 → sqrt MAX−1, `is_base_input` 1.
/// - **Whirlpool** (0x04) `swap_v2`: 49 B. Sqrt 0, `a_to_b` = direction byte,
///   `RemainingAccountsInfo` = one SupplementalTickArrays slice of length 2 (682/682).
/// - **Whirlpool v1** (0x25) `swap`: 42 B. Direction 1 → sqrt MIN and `a_to_b` 1,
///   direction 0 → sqrt MAX and `a_to_b` 0.
/// - **Pump sell** (0x05, direction 0): 24 B, threshold = `min_out` (3,197/3,197 are 1).
/// - **Pump buy** (0x05, direction 1): `buy_exact_quote_in`, 25 B, threshold = `min_out`, byte `0x01`
///   (4,058/4,058). Exact-out `buy` does not occur in the window.
/// - **DAMM v2** (0x08) and **DAMM v1** (0x00) `swap`: 24 B, threshold 0 (1,266/1,266 and 81/81). Not `swap2`.
/// - **Byreal** (0x0a): 41 B, sqrt 0, last byte 1 (98/98). The direction is not in the data.
/// - **Futarchy** (0x16) `spot_swap`: 25 B. Byte after the amount = route direction, then u64 0 (372/372).
/// - **DFlow** (0x17) `swap2`: 35 B. Vector of length 1, step `0x28`, amount, threshold 0,
///   byte 0, slippage 10,000, byte 0 (913/913). The route direction is always 0.
/// - **Raydium AMM v4** (0x26): tag `0x10`, amount, threshold 0, 17 B (245/245).
/// - **Manifest** (0x10): `0x0d | amount_in | 0 | direction | 0x01`, 19 B (3,261/3,261).
/// - **Pump.fun** (0x0e): direction 0 = `buy_exact_quote_in_v2`, direction 1 = `sell_v2`,
///   24 B, second u64 = `min_out` (1 in the window). 22/22 and 11/11.
/// - **LaunchLab** (0x13): direction 0 = `buy_exact_in`, direction 1 = `sell_exact_in`,
///   32 B, 16 zero bytes after the amount. 11/11 and 13/13.
/// - **GoonFi** (0x1b): `01 | (1 − direction) | amount | 8×0`, 18 B.
/// - **Orca token-swap** (0x15), **Saros** (0x0d), **SwaPpA9L** (0x1d): tag `0x01`, amount, threshold 0, 17 B.
/// - **Hylo** (0x29): the discriminator depends on the direction, amount, byte 0, 17 B.
/// - **BSwp** (0x14): direction 0 only. `swap`, amount, u128 MAX, byte 0, 33 B.
/// - **AlphaQ** (0x21): direction 0 only. `0c 00 | amount | 8×0`, 18 B.
/// - **Meteora DBC** (0x12): same 24 B as DAMM `swap`. One failed sample.
/// HumidiFi (0x0b) has XOR data, and QuaNt (0x28) is not a leg of tag 0x0a. Both return `None`.
pub fn encode_swap(venue: u8, direction: u8, amount_in: u64, min_out: u64, buf: &mut [u8]) -> Option<usize> {
    match venue {
        venue::RAYDIUM_CPMM => {
            let o = put(buf, 0, &disc::SWAP_BASE_INPUT)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::RAYDIUM_CLMM | venue::PANCAKE_CLMM => {
            let o = put(buf, 0, &disc::SWAP_V2)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &0u128.to_le_bytes())?;
            let o = put(buf, o, &[1u8])?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::RAYDIUM_CLMM_V1 | venue::PANCAKE_CLMM_V1 => {
            let sqrt = match direction {
                1 => RAYDIUM_SQRT_MIN_PLUS_ONE,
                0 => RAYDIUM_SQRT_MAX_MINUS_ONE,
                _ => return None,
            };
            let o = put(buf, 0, &disc::SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &sqrt.to_le_bytes())?;
            put(buf, o, &[1u8])
        }
        venue::DLMM => {
            let o = put(buf, 0, &disc::SWAP2)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            put(buf, o, &0u32.to_le_bytes())
        }
        venue::DLMM_V1 => {
            let o = put(buf, 0, &disc::SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::WHIRLPOOL => {
            if direction > 1 {
                return None;
            }
            let o = put(buf, 0, &disc::SWAP_V2)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &0u128.to_le_bytes())?;
            let o = put(buf, o, &[1u8])?;
            let o = put(buf, o, &[direction])?;
            // Option::Some, one slice: SupplementalTickArrays = 6, length 2.
            let o = put(buf, o, &[1u8])?;
            let o = put(buf, o, &1u32.to_le_bytes())?;
            let o = put(buf, o, &[6u8])?;
            put(buf, o, &[2u8])
        }
        venue::WHIRLPOOL_V1 => {
            let sqrt = match direction {
                1 => WHIRLPOOL_MIN_SQRT_PRICE,
                0 => WHIRLPOOL_MAX_SQRT_PRICE,
                _ => return None,
            };
            let o = put(buf, 0, &disc::SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &sqrt.to_le_bytes())?;
            let o = put(buf, o, &[1u8])?;
            put(buf, o, &[direction])
        }
        venue::PUMP_AMM => {
            if direction == DIR_A_TO_B {
                let o = put(buf, 0, &disc::SELL)?;
                let o = put(buf, o, &amount_in.to_le_bytes())?;
                put(buf, o, &min_out.to_le_bytes())
            } else if direction == 1 {
                let o = put(buf, 0, &disc::BUY_EXACT_QUOTE_IN)?;
                let o = put(buf, o, &amount_in.to_le_bytes())?;
                let o = put(buf, o, &min_out.to_le_bytes())?;
                put(buf, o, &[1u8])
            } else {
                None
            }
        }
        venue::DAMM_V2 | venue::DAMM_V1 | venue::METEORA_DBC => {
            let o = put(buf, 0, &disc::SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::PUMP_FUN => {
            let disc = match direction {
                0 => disc::BUY_EXACT_QUOTE_IN_V2,
                1 => disc::SELL_V2,
                _ => return None,
            };
            let o = put(buf, 0, &disc)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &min_out.to_le_bytes())
        }
        venue::LAUNCHPAD => {
            let disc = match direction {
                0 => disc::BUY_EXACT_IN,
                1 => disc::SELL_EXACT_IN,
                _ => return None,
            };
            let o = put(buf, 0, &disc)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &[0u8; 16])
        }
        venue::GOONFI => {
            let side = match direction {
                0 => 1u8,
                1 => 0u8,
                _ => return None,
            };
            let o = put(buf, 0, &[0x01, side])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &[0u8; 8])
        }
        venue::ORCA_TOKEN_SWAP | venue::SAROS | venue::SWAP_A9 => {
            if direction > 1 {
                return None;
            }
            let o = put(buf, 0, &[0x01])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::HYLO => {
            let disc = match direction {
                0 => disc::HYLO_0,
                1 => disc::HYLO_1,
                _ => return None,
            };
            let o = put(buf, 0, &disc)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &[0u8])
        }
        venue::BSWP => {
            if direction != 0 {
                return None;
            }
            let o = put(buf, 0, &disc::SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &u128::MAX.to_le_bytes())?;
            put(buf, o, &[0u8])
        }
        venue::ALPHAQ => {
            if direction != 0 {
                return None;
            }
            let o = put(buf, 0, &[0x0c, 0x00])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &[0u8; 8])
        }
        venue::BYREAL => {
            let o = put(buf, 0, &disc::BYREAL_SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &0u128.to_le_bytes())?;
            put(buf, o, &[1u8])
        }
        venue::FUTARCHY => {
            if direction > 1 {
                return None;
            }
            let o = put(buf, 0, &disc::SPOT_SWAP)?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &[direction])?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::DFLOW => {
            let o = put(buf, 0, &disc::SWAP2)?;
            let o = put(buf, o, &1u32.to_le_bytes())?;
            let o = put(buf, o, &[0x28])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &[0u8])?;
            let o = put(buf, o, &10_000u32.to_le_bytes())?;
            put(buf, o, &[0u8])
        }
        venue::RAYDIUM_V4 => {
            let o = put(buf, 0, &[0x10])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            put(buf, o, &0u64.to_le_bytes())
        }
        venue::MANIFEST => {
            if direction > 1 {
                return None;
            }
            let o = put(buf, 0, &[MANIFEST_SWAP_V2])?;
            let o = put(buf, o, &amount_in.to_le_bytes())?;
            let o = put(buf, o, &0u64.to_le_bytes())?;
            let o = put(buf, o, &[direction])?;
            put(buf, o, &[1u8])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminators_match_anchor_sha256() {
        // Anchor discriminator = sha256("global:<name>")[..8]; verified via keys::sha256.
        let d = |name: &str| -> [u8; 8] {
            let h = keys::sha256(format!("global:{name}").as_bytes());
            let mut out = [0u8; 8];
            out.copy_from_slice(&h[..8]);
            out
        };
        assert_eq!(disc::SWAP2, d("swap2"));
        assert_eq!(disc::SWAP_V2, d("swap_v2"));
        assert_eq!(disc::SWAP_BASE_INPUT, d("swap_base_input"));
        assert_eq!(disc::SWAP, d("swap"));
        assert_eq!(disc::SPOT_SWAP, d("spot_swap"));
        assert_eq!(disc::BUY, d("buy"));
        assert_eq!(disc::SELL, d("sell"));
        assert_eq!(disc::BUY_EXACT_QUOTE_IN_V2, d("buy_exact_quote_in_v2"));
        assert_eq!(disc::SELL_V2, d("sell_v2"));
        assert_eq!(disc::BUY_EXACT_IN, d("buy_exact_in"));
        assert_eq!(disc::SELL_EXACT_IN, d("sell_exact_in"));
    }

    #[test]
    fn encodes_cpmm_exact_in() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::RAYDIUM_CPMM, 0, 1_000_000, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SWAP_BASE_INPUT);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 1_000_000);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);
    }

    #[test]
    fn encodes_clmm_swap_v2() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::RAYDIUM_CLMM, 0, 500, 1, &mut buf).unwrap();
        assert_eq!(n, 49);
        assert_eq!(&buf[..8], &disc::SWAP_V2);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 500);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), 0);
        assert_eq!(buf[40], 1);
        assert_eq!(&buf[41..49], &[0u8; 8]);
        assert_eq!(encode_swap(venue::PANCAKE_CLMM, 0, 500, 1, &mut buf), Some(49));
    }

    #[test]
    fn encodes_dlmm_swap2_with_empty_remaining() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::DLMM, 0, 7, 1, &mut buf).unwrap();
        assert_eq!(n, 28);
        assert_eq!(&buf[..8], &disc::SWAP2);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(buf[24..28].try_into().unwrap()), 0);
    }

    #[test]
    fn encodes_whirlpool_swap_v2() {
        let mut buf = [0u8; 64];
        // Direction 0: a_to_b = 0. Sqrt 0. SupplementalTickArrays slice, length 2.
        let n = encode_swap(venue::WHIRLPOOL, 0, 123, 1, &mut buf).unwrap();
        assert_eq!(n, 49);
        assert_eq!(&buf[..8], &disc::SWAP_V2);
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), 0);
        assert_eq!(buf[40], 1);
        assert_eq!(buf[41], 0);
        assert_eq!(&buf[42..49], &[1, 1, 0, 0, 0, 6, 2]);
        let n = encode_swap(venue::WHIRLPOOL, 1, 123, 1, &mut buf).unwrap();
        assert_eq!(n, 49);
        assert_eq!(buf[41], 1);
    }

    #[test]
    fn encodes_pump_sell_exact_in() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::PUMP_AMM, 0, 9_000, 1, &mut buf).unwrap();
        assert_eq!(n, 8 + 8 + 8);
        assert_eq!(&buf[..8], &disc::SELL);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 9_000); // base_amount_in
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 1); // min_quote_amount_out
    }

    #[test]
    fn encodes_pump_buy_exact_quote_in() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::PUMP_AMM, 1, 9_000, 1, &mut buf).unwrap();
        assert_eq!(n, 25);
        assert_eq!(&buf[..8], &disc::BUY_EXACT_QUOTE_IN);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 9_000);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 1);
        assert_eq!(buf[24], 1);
    }

    #[test]
    fn encodes_damm_v2_swap() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::DAMM_V2, 0, 4_200, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SWAP);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 4_200);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);
    }

    #[test]
    fn encodes_manifest_swap_v2() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::MANIFEST, 1, 9_000, 1, &mut buf).unwrap();
        assert_eq!(n, 19);
        assert_eq!(buf[0], 0x0d);
        assert_eq!(u64::from_le_bytes(buf[1..9].try_into().unwrap()), 9_000);
        assert_eq!(u64::from_le_bytes(buf[9..17].try_into().unwrap()), 0);
        assert_eq!(buf[17], 1);
        assert_eq!(buf[18], 1);
        assert_eq!(encode_swap(venue::MANIFEST, 0, 9_000, 1, &mut buf), Some(19));
        assert_eq!(buf[17], 0);
    }

    #[test]
    fn encodes_legacy_swap_venues() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::DLMM_V1, 0, 7, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SWAP);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);

        let n = encode_swap(venue::RAYDIUM_CLMM_V1, 1, 5, 1, &mut buf).unwrap();
        assert_eq!(n, 41);
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), RAYDIUM_SQRT_MIN_PLUS_ONE);
        assert_eq!(buf[40], 1);
        let n = encode_swap(venue::PANCAKE_CLMM_V1, 0, 5, 1, &mut buf).unwrap();
        assert_eq!(n, 41);
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), RAYDIUM_SQRT_MAX_MINUS_ONE);

        let n = encode_swap(venue::WHIRLPOOL_V1, 1, 5, 1, &mut buf).unwrap();
        assert_eq!(n, 42);
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), WHIRLPOOL_MIN_SQRT_PRICE);
        assert_eq!(buf[40], 1);
        assert_eq!(buf[41], 1);
        assert_eq!(encode_swap(venue::WHIRLPOOL_V1, 0, 5, 1, &mut buf), Some(42));
        assert_eq!(u128::from_le_bytes(buf[24..40].try_into().unwrap()), WHIRLPOOL_MAX_SQRT_PRICE);
        assert_eq!(buf[41], 0);
    }

    #[test]
    fn encodes_window_venues_without_a_planner() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::DAMM_V1, 1, 4_200, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SWAP);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);

        let n = encode_swap(venue::BYREAL, 0, 9, 1, &mut buf).unwrap();
        assert_eq!(n, 41);
        assert_eq!(&buf[..8], &disc::BYREAL_SWAP);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 9);
        assert_eq!(&buf[16..40], &[0u8; 24]);
        assert_eq!(buf[40], 1);

        let n = encode_swap(venue::FUTARCHY, 1, 3, 1, &mut buf).unwrap();
        assert_eq!(n, 25);
        assert_eq!(&buf[..8], &disc::SPOT_SWAP);
        assert_eq!(buf[16], 1);
        assert_eq!(&buf[17..25], &[0u8; 8]);
        assert_eq!(encode_swap(venue::FUTARCHY, 0, 3, 1, &mut buf), Some(25));
        assert_eq!(buf[16], 0);

        let n = encode_swap(venue::DFLOW, 0, 0x075f_3a36, 1, &mut buf).unwrap();
        assert_eq!(n, 35);
        assert_eq!(&buf[..8], &disc::SWAP2);
        assert_eq!(u32::from_le_bytes(buf[8..12].try_into().unwrap()), 1);
        assert_eq!(buf[12], 0x28);
        assert_eq!(u64::from_le_bytes(buf[13..21].try_into().unwrap()), 0x075f_3a36);
        assert_eq!(&buf[21..29], &[0u8; 8]);
        assert_eq!(buf[29], 0);
        assert_eq!(u32::from_le_bytes(buf[30..34].try_into().unwrap()), 10_000);
        assert_eq!(buf[34], 0);

        let n = encode_swap(venue::RAYDIUM_V4, 1, 99, 1, &mut buf).unwrap();
        assert_eq!(n, 17);
        assert_eq!(buf[0], 0x10);
        assert_eq!(u64::from_le_bytes(buf[1..9].try_into().unwrap()), 99);
        assert_eq!(&buf[9..17], &[0u8; 8]);
    }

    #[test]
    fn unsupported_returns_none() {
        let mut buf = [0u8; 64];
        assert_eq!(encode_swap(0x11, 0, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::PUMP_AMM, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::MANIFEST, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::WHIRLPOOL, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::WHIRLPOOL_V1, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::RAYDIUM_CLMM_V1, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::FUTARCHY, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::PUMP_FUN, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::LAUNCHPAD, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::GOONFI, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::HYLO, 2, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::BSWP, 1, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(venue::ALPHAQ, 1, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(0x0b, 0, 1, 1, &mut buf), None);
        assert_eq!(encode_swap(0x28, 0, 1, 1, &mut buf), None);
    }

    #[test]
    fn encodes_small_window_venues() {
        let mut buf = [0u8; 64];
        let n = encode_swap(venue::PUMP_FUN, 0, 9_000, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::BUY_EXACT_QUOTE_IN_V2);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 9_000);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 1);
        let n = encode_swap(venue::PUMP_FUN, 1, 9_000, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SELL_V2);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 1);

        let n = encode_swap(venue::LAUNCHPAD, 0, 5, 1, &mut buf).unwrap();
        assert_eq!(n, 32);
        assert_eq!(&buf[..8], &disc::BUY_EXACT_IN);
        assert_eq!(&buf[16..32], &[0u8; 16]);
        let n = encode_swap(venue::LAUNCHPAD, 1, 5, 1, &mut buf).unwrap();
        assert_eq!(n, 32);
        assert_eq!(&buf[..8], &disc::SELL_EXACT_IN);

        let n = encode_swap(venue::GOONFI, 0, 0x2f_939f_b0, 1, &mut buf).unwrap();
        assert_eq!(n, 18);
        assert_eq!(buf[0], 0x01);
        assert_eq!(buf[1], 1);
        assert_eq!(u64::from_le_bytes(buf[2..10].try_into().unwrap()), 0x2f_939f_b0);
        assert_eq!(&buf[10..18], &[0u8; 8]);
        let n = encode_swap(venue::GOONFI, 1, 7, 1, &mut buf).unwrap();
        assert_eq!(n, 18);
        assert_eq!(buf[1], 0);

        let n = encode_swap(venue::ORCA_TOKEN_SWAP, 0, 0x011b_2765, 1, &mut buf).unwrap();
        assert_eq!(n, 17);
        assert_eq!(buf[0], 0x01);
        assert_eq!(u64::from_le_bytes(buf[1..9].try_into().unwrap()), 0x011b_2765);
        assert_eq!(&buf[9..17], &[0u8; 8]);
        assert_eq!(encode_swap(venue::SAROS, 1, 3, 1, &mut buf), Some(17));
        assert_eq!(encode_swap(venue::SWAP_A9, 0, 3, 1, &mut buf), Some(17));

        let n = encode_swap(venue::HYLO, 0, 4, 1, &mut buf).unwrap();
        assert_eq!(n, 17);
        assert_eq!(&buf[..8], &disc::HYLO_0);
        assert_eq!(buf[16], 0);
        let n = encode_swap(venue::HYLO, 1, 4, 1, &mut buf).unwrap();
        assert_eq!(n, 17);
        assert_eq!(&buf[..8], &disc::HYLO_1);
        assert_eq!(buf[16], 0);

        let n = encode_swap(venue::BSWP, 0, 9, 1, &mut buf).unwrap();
        assert_eq!(n, 33);
        assert_eq!(&buf[..8], &disc::SWAP);
        assert_eq!(u128::from_le_bytes(buf[16..32].try_into().unwrap()), u128::MAX);
        assert_eq!(buf[32], 0);

        let n = encode_swap(venue::ALPHAQ, 0, 0x01c4, 1, &mut buf).unwrap();
        assert_eq!(n, 18);
        assert_eq!(&buf[..2], &[0x0c, 0x00]);
        assert_eq!(u64::from_le_bytes(buf[2..10].try_into().unwrap()), 0x01c4);
        assert_eq!(&buf[10..18], &[0u8; 8]);

        let n = encode_swap(venue::METEORA_DBC, 0, 33_528_396, 1, &mut buf).unwrap();
        assert_eq!(n, 24);
        assert_eq!(&buf[..8], &disc::SWAP);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 0);
    }

    #[test]
    fn discriminators_match_real_mainnet() {
        // Discriminators collected from real mainnet instructions.
        assert_eq!(disc::SWAP_BASE_INPUT, [0x8f, 0xbe, 0x5a, 0xda, 0xc4, 0x1e, 0x33, 0xde]); // CPMM
        assert_eq!(disc::SWAP_V2, [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62]); // CLMM/Whirlpool swap_v2
        assert_eq!(disc::SWAP2, [0x41, 0x4b, 0x3f, 0x4c, 0xeb, 0x5b, 0x5b, 0x88]); // DLMM/DAMM v2 swap2
        assert_eq!(disc::BUY, [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]); // Pump buy exact-out, absent from the window
        assert_eq!(disc::BUY_EXACT_QUOTE_IN, [0xc6, 0x2e, 0x15, 0x52, 0xb4, 0xd9, 0xe8, 0x70]);
        assert_eq!(disc::SELL, [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad]); // Pump sell
    }

    #[test]
    fn buffer_too_small_is_none() {
        let mut buf = [0u8; 10];
        assert_eq!(encode_swap(venue::RAYDIUM_CPMM, 0, 1, 1, &mut buf), None);
    }
}
