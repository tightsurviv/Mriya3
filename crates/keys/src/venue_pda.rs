//! Derivation of derived PDA accounts per venue (oracle, bin_array, tick_array, observation,
//! Manifest global, Pump volume accumulator) and the index math for variable tails.
//!
//! Seeds determined **empirically on real accounts of the reference bot** (matching the computed
//! address to an account from the fixture). The route's intermediate accounts are **not** ATAs — the bot creates them per
//! transaction — so they are not here; the route planner supplies them. ATAs are computed by [`crate::associated_token_address`].

use crate::{find_program_address, Pubkey};

/// Meteora DLMM `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`.
pub const DLMM: Pubkey = [4, 233, 225, 47, 188, 132, 232, 38, 201, 50, 204, 233, 226, 100, 12, 206, 21, 89, 12, 28, 98, 115, 176, 146, 87, 8, 186, 59, 133, 32, 176, 188];
/// Raydium CLMM `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`.
pub const CLMM: Pubkey = [165, 213, 202, 158, 4, 207, 93, 181, 144, 183, 20, 186, 47, 227, 44, 177, 89, 19, 63, 193, 193, 146, 183, 34, 87, 253, 7, 211, 156, 176, 64, 30];
/// Pancake CLMM `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`. Same seeds as Raydium.
pub const PANCAKE: Pubkey = [249, 221, 203, 31, 226, 248, 142, 187, 172, 95, 91, 151, 180, 185, 10, 212, 149, 81, 150, 43, 137, 65, 157, 118, 82, 132, 98, 40, 69, 228, 28, 248];
/// Byreal CLMM `REALQqNEomY6cQGZJUGwywTBD2UmDT32rZcNnfxQ5N2`. Same seeds as Raydium.
pub const BYREAL: Pubkey = [6, 52, 210, 15, 27, 245, 109, 209, 100, 227, 193, 60, 120, 54, 164, 34, 158, 197, 171, 33, 255, 84, 82, 131, 144, 165, 23, 137, 243, 205, 187, 219];
/// Raydium CPMM `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`.
pub const CPMM: Pubkey = [169, 42, 90, 139, 79, 41, 89, 82, 132, 37, 80, 170, 147, 253, 91, 149, 181, 172, 230, 168, 235, 146, 12, 147, 148, 46, 67, 105, 12, 32, 236, 115];
/// Orca Whirlpool `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`.
pub const WHIRLPOOL: Pubkey = [14, 3, 104, 95, 142, 144, 144, 83, 228, 88, 18, 28, 102, 245, 167, 106, 237, 199, 112, 106, 161, 28, 130, 248, 170, 149, 42, 143, 43, 120, 121, 169];
/// Manifest `MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms`.
pub const MANIFEST: Pubkey = [5, 55, 164, 119, 56, 218, 37, 169, 136, 249, 100, 217, 155, 164, 58, 136, 207, 17, 135, 57, 91, 106, 207, 20, 131, 245, 112, 34, 141, 128, 2, 178];
/// Pump AMM `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`.
pub const PUMP: Pubkey = [12, 20, 222, 252, 130, 94, 198, 118, 148, 37, 8, 24, 187, 101, 64, 101, 244, 41, 141, 49, 86, 213, 113, 180, 212, 248, 9, 12, 24, 233, 168, 99];

/// Number of bins per DLMM array (BinArray).
pub const DLMM_BINS_PER_ARRAY: i64 = 70;
/// Number of ticks per Raydium CLMM array (TickArray).
pub const CLMM_TICKS_PER_ARRAY: i32 = 60;

/// DLMM oracle: `PDA(["oracle", lb_pair])`.
pub fn dlmm_oracle(lb_pair: &Pubkey) -> Pubkey {
    find_program_address(&[b"oracle", lb_pair], &DLMM).0
}

/// DLMM BinArray: `PDA(["bin_array", lb_pair, i64_le(index)])`.
pub fn dlmm_bin_array(lb_pair: &Pubkey, index: i64) -> Pubkey {
    let ib = index.to_le_bytes();
    find_program_address(&[b"bin_array", lb_pair, &ib], &DLMM).0
}

/// Index of the BinArray containing the given bin: `bin_id.div_euclid(70)` (floor for negatives too).
pub fn dlmm_bin_array_index(bin_id: i32) -> i64 {
    (bin_id as i64).div_euclid(DLMM_BINS_PER_ARRAY)
}

/// CLMM observation: `PDA(["observation", pool_state])` of the pool program.
/// Raydium, Pancake and Byreal use this seed.
pub fn clmm_observation_of(program: &Pubkey, pool_state: &Pubkey) -> Pubkey {
    find_program_address(&[b"observation", pool_state], program).0
}

/// Raydium CLMM observation (oracle TWAP): `PDA(["observation", pool_state])`.
pub fn clmm_observation(pool_state: &Pubkey) -> Pubkey {
    clmm_observation_of(&CLMM, pool_state)
}

/// Tick array bitmap extension: `PDA(["pool_tick_array_bitmap_extension", pool_state])`.
pub fn clmm_bitmap_extension_of(program: &Pubkey, pool_state: &Pubkey) -> Pubkey {
    find_program_address(&[b"pool_tick_array_bitmap_extension", pool_state], program).0
}

/// Raydium CLMM tick array bitmap extension.
/// First account of the Raydium variable tail (before the tick arrays). In Pancake v1 it is last.
pub fn clmm_bitmap_extension(pool_state: &Pubkey) -> Pubkey {
    clmm_bitmap_extension_of(&CLMM, pool_state)
}

/// TickArray: `PDA(["tick_array", pool_state, i32_be(start_index)])`.
pub fn clmm_tick_array_of(program: &Pubkey, pool_state: &Pubkey, start_index: i32) -> Pubkey {
    let sb = start_index.to_be_bytes();
    find_program_address(&[b"tick_array", pool_state, &sb], program).0
}

/// Raydium CLMM TickArray: `PDA(["tick_array", pool_state, i32_be(start_index)])`.
pub fn clmm_tick_array(pool_state: &Pubkey, start_index: i32) -> Pubkey {
    clmm_tick_array_of(&CLMM, pool_state, start_index)
}

/// Start index of the TickArray containing `tick`: quantized down to `tick_spacing·60`.
pub fn clmm_tick_array_start(tick: i32, tick_spacing: u16) -> i32 {
    let span = tick_spacing as i32 * CLMM_TICKS_PER_ARRAY;
    tick.div_euclid(span) * span
}

/// Sequence of TickArray starts for a swap: from the array with `current_tick` in the swap direction, `count` of them.
/// `ascending=true` when the price rises (tick up), `false` when it falls. (How many to actually pass —
/// depends on the initialized arrays in the pool bitmap; here we generate a contiguous window.)
pub fn clmm_tick_array_starts(current_tick: i32, tick_spacing: u16, ascending: bool, count: u32) -> Vec<i32> {
    let span = tick_spacing as i32 * CLMM_TICKS_PER_ARRAY;
    let start0 = current_tick.div_euclid(span) * span;
    (0..count as i32).map(|i| if ascending { start0 + i * span } else { start0 - i * span }).collect()
}

/// Sequence of BinArray indices for a swap: from the array with `active_id` in the swap direction, `count` of them.
/// `ascending=true` when active_id rises (swap side raising the id), `false` when it falls.
pub fn dlmm_bin_array_indices(active_id: i32, ascending: bool, count: u32) -> Vec<i64> {
    let idx0 = dlmm_bin_array_index(active_id);
    (0..count as i64).map(|i| if ascending { idx0 + i } else { idx0 - i }).collect()
}

/// Raydium CPMM observation: `PDA(["observation", pool_state])`.
pub fn cpmm_observation(pool_state: &Pubkey) -> Pubkey {
    find_program_address(&[b"observation", pool_state], &CPMM).0
}

/// Orca Whirlpool oracle (adaptive-fee): `PDA(["oracle", whirlpool])`.
pub fn whirlpool_oracle(whirlpool: &Pubkey) -> Pubkey {
    find_program_address(&[b"oracle", whirlpool], &WHIRLPOOL).0
}

/// Ticks in a single Orca array. Raydium has 60; here it is 88.
pub const WHIRLPOOL_TICKS_PER_ARRAY: i32 = 88;

/// TickArray Whirlpool: `PDA(["tick_array", whirlpool, start_index as text])`.
/// A negative index goes in with a minus sign (`"-88"`), not as an `i32` big-endian.
pub fn whirlpool_tick_array(whirlpool: &Pubkey, start_index: i32) -> Pubkey {
    let text = start_index.to_string();
    find_program_address(&[b"tick_array", whirlpool, text.as_bytes()], &WHIRLPOOL).0
}

/// Start of the array containing `tick`: quantized down to `tick_spacing · 88`.
pub fn whirlpool_tick_array_start(tick: i32, tick_spacing: u16) -> i32 {
    let span = tick_spacing as i32 * WHIRLPOOL_TICKS_PER_ARRAY;
    tick.div_euclid(span) * span
}

/// Three arrays of `swap` v1 (0x25), in the swap direction. `ascending` = not a→b.
pub fn whirlpool_tick_array_starts(current_tick: i32, tick_spacing: u16, ascending: bool, count: u32) -> Vec<i32> {
    let span = tick_spacing as i32 * WHIRLPOOL_TICKS_PER_ARRAY;
    let start0 = current_tick.div_euclid(span) * span;
    (0..count as i32).map(|i| if ascending { start0 + i * span } else { start0 - i * span }).collect()
}

/// Window of `swap_v2` (0x04) from successful legs: three arrays up from the current one and two down.
/// The swap direction does not change this layout.
pub fn whirlpool_v2_window(current_tick: i32, tick_spacing: u16) -> (Vec<i32>, Vec<i32>) {
    let span = tick_spacing as i32 * WHIRLPOOL_TICKS_PER_ARRAY;
    let start0 = current_tick.div_euclid(span) * span;
    (
        vec![start0, start0 + span, start0 + 2 * span],
        vec![start0 - span, start0 - 2 * span],
    )
}

/// Manifest global (per mint quote): `PDA(["global", quote_mint])`.
pub fn manifest_global(quote_mint: &Pubkey) -> Pubkey {
    find_program_address(&[b"global", quote_mint], &MANIFEST).0
}

/// Manifest global vault: `PDA(["global-vault", quote_mint])`.
pub fn manifest_global_vault(quote_mint: &Pubkey) -> Pubkey {
    find_program_address(&[b"global-vault", quote_mint], &MANIFEST).0
}

/// Pump AMM user volume accumulator: `PDA(["user_volume_accumulator", user])`.
pub fn pump_user_volume_accumulator(user: &Pubkey) -> Pubkey {
    find_program_address(&[b"user_volume_accumulator", user], &PUMP).0
}

#[cfg(test)]
mod tests {
    use super::*;

    const DLMM_LBPAIR: Pubkey = [121, 31, 29, 120, 38, 16, 190, 35, 165, 114, 28, 232, 2, 235, 69, 172, 5, 11, 69, 125, 67, 62, 215, 49, 88, 160, 163, 18, 68, 128, 130, 143];
    const DLMM_ORACLE: Pubkey = [232, 102, 237, 29, 233, 133, 117, 81, 138, 239, 118, 1, 105, 36, 73, 53, 206, 225, 174, 81, 75, 63, 246, 16, 50, 102, 50, 195, 144, 5, 161, 48];
    const DLMM_BINARRAY: Pubkey = [167, 106, 246, 109, 168, 10, 249, 194, 153, 64, 172, 17, 58, 126, 217, 210, 18, 11, 160, 153, 193, 100, 224, 97, 19, 251, 24, 12, 135, 82, 36, 8];
    const CLMM_POOL: Pubkey = [50, 215, 55, 91, 63, 118, 195, 112, 178, 122, 192, 111, 240, 213, 98, 246, 43, 47, 215, 247, 149, 111, 34, 252, 215, 68, 157, 180, 76, 13, 29, 132];
    const CLMM_OBS: Pubkey = [155, 170, 238, 33, 182, 109, 115, 167, 176, 244, 234, 47, 106, 15, 169, 197, 227, 157, 99, 72, 92, 232, 209, 40, 243, 53, 235, 77, 200, 12, 223, 177];
    const CLMM_TICKARR: Pubkey = [228, 36, 111, 60, 230, 32, 211, 31, 227, 71, 205, 175, 134, 110, 88, 144, 139, 240, 22, 174, 96, 69, 105, 159, 216, 236, 72, 251, 196, 103, 4, 89];
    const CLMM_BITMAP_EXT: Pubkey = [201, 102, 204, 73, 169, 206, 199, 78, 242, 95, 88, 255, 21, 187, 5, 16, 43, 31, 40, 244, 233, 249, 228, 253, 131, 123, 51, 240, 191, 180, 87, 94];
    const WP_POOL: Pubkey = [230, 43, 238, 7, 101, 105, 91, 104, 175, 245, 50, 188, 73, 207, 193, 168, 136, 91, 122, 1, 212, 190, 190, 197, 74, 133, 76, 53, 92, 88, 162, 118];
    const WP_ORACLE: Pubkey = [119, 113, 59, 44, 121, 148, 188, 25, 65, 160, 216, 160, 246, 7, 157, 159, 245, 206, 49, 253, 143, 179, 37, 23, 77, 185, 234, 112, 96, 37, 172, 205];
    const CPMM_POOL: Pubkey = [53, 38, 92, 67, 216, 207, 121, 22, 200, 154, 122, 166, 136, 141, 205, 122, 7, 192, 12, 46, 64, 188, 162, 241, 171, 95, 182, 114, 131, 76, 122, 1];
    const CPMM_OBS: Pubkey = [64, 168, 179, 228, 192, 103, 229, 48, 93, 59, 101, 192, 136, 92, 32, 151, 175, 150, 238, 147, 114, 37, 101, 255, 28, 203, 0, 143, 244, 117, 111, 231];
    const MF_QUOTEMINT: Pubkey = [6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26, 235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1];
    const MF_GLOBAL: Pubkey = [100, 134, 218, 226, 246, 16, 50, 62, 155, 230, 21, 98, 207, 252, 251, 22, 181, 55, 28, 253, 50, 58, 150, 31, 45, 16, 235, 171, 60, 180, 123, 242];
    const MF_GLOBALVAULT: Pubkey = [3, 85, 105, 108, 247, 217, 68, 53, 2, 87, 133, 26, 201, 25, 54, 20, 200, 157, 107, 17, 226, 63, 83, 161, 232, 252, 65, 20, 43, 197, 158, 151];
    const PUMP_USER: Pubkey = [5, 87, 214, 198, 98, 247, 219, 155, 23, 158, 96, 49, 199, 215, 83, 103, 73, 11, 6, 14, 71, 34, 167, 70, 233, 126, 82, 150, 103, 249, 236, 34];
    const PUMP_UVA: Pubkey = [251, 117, 74, 235, 13, 164, 125, 164, 37, 74, 59, 10, 216, 54, 220, 8, 122, 136, 98, 247, 157, 115, 207, 2, 149, 113, 9, 0, 20, 92, 221, 64];

    fn hop_keys(hex: &str) -> Vec<Pubkey> {
        let raw = hex.trim();
        let bytes: Vec<u8> = (0..raw.len()).step_by(2).map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap()).collect();
        (0..bytes.len() / 32)
            .map(|i| {
                let mut k = [0u8; 32];
                k.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
                k
            })
            .collect()
    }

    #[test]
    fn whirlpool_tick_arrays_match_real_hops() {
        // Successful swap_v2, direction 1. The five arrays are a window around the current one, not in the swap direction.
        let v2 = hop_keys(include_str!("../../send/src/testdata/whirlpool_hop_accounts.hex"));
        assert_eq!(v2.len(), 18);
        let pool = v2[5];
        assert_eq!(whirlpool_oracle(&pool), v2[15]);
        assert_eq!(whirlpool_tick_array(&pool, 45_056), v2[12]);
        assert_eq!(whirlpool_tick_array(&pool, 67_584), v2[13]);
        assert_eq!(whirlpool_tick_array(&pool, 90_112), v2[14]);
        assert_eq!(whirlpool_tick_array(&pool, 22_528), v2[16]);
        assert_eq!(whirlpool_tick_array(&pool, 0), v2[17]);
        let (core, supp) = whirlpool_v2_window(45_056 + 10, 256);
        assert_eq!(core, vec![45_056, 67_584, 90_112]);
        assert_eq!(supp, vec![22_528, 0]);

        // Successful swap v1, direction 1 (a→b): three arrays down. spacing 16 → step 1408.
        let v1 = hop_keys(include_str!("../../send/src/testdata/whirlpool_v1_hop_accounts.hex"));
        assert_eq!(v1.len(), 12);
        let pool = v1[3];
        assert_eq!(whirlpool_oracle(&pool), v1[11]);
        let starts = whirlpool_tick_array_starts(73_216, 16, false, 3);
        assert_eq!(starts, vec![73_216, 71_808, 70_400]);
        assert_eq!(whirlpool_tick_array(&pool, starts[0]), v1[8]);
        assert_eq!(whirlpool_tick_array(&pool, starts[1]), v1[9]);
        assert_eq!(whirlpool_tick_array(&pool, starts[2]), v1[10]);
        assert_eq!(whirlpool_tick_array_start(-1, 16), -1_408);
        assert_eq!(whirlpool_tick_array_starts(0, 8, true, 3), vec![0, 704, 1_408]);
    }

    #[test]
    fn dlmm_pdas_match_real() {
        assert_eq!(dlmm_oracle(&DLMM_LBPAIR), DLMM_ORACLE);
        // index -11 reproduced from a real tx (= active_id.div_euclid(70) for active_id in [-770,-701]).
        assert_eq!(dlmm_bin_array(&DLMM_LBPAIR, -11), DLMM_BINARRAY);
        assert_eq!(dlmm_bin_array_index(-770), -11);
        assert_eq!(dlmm_bin_array_index(-701), -11);
        assert_eq!(dlmm_bin_array_index(0), 0);
        assert_eq!(dlmm_bin_array_index(-1), -1);
    }

    #[test]
    fn clmm_pdas_match_real() {
        assert_eq!(clmm_observation(&CLMM_POOL), CLMM_OBS);
        // CLMM tail = [bitmap_extension, tick_array…]. Starts from a real tx (tick_spacing 10 → step 600).
        assert_eq!(clmm_bitmap_extension(&CLMM_POOL), CLMM_BITMAP_EXT);
        assert_eq!(clmm_tick_array(&CLMM_POOL, -55200), CLMM_TICKARR);
        assert_eq!(clmm_tick_array_start(-55150, 10), -55200);
        assert_eq!(clmm_tick_array_start(-54600, 10), -54600);
        assert_eq!(clmm_tick_array_start(0, 10), 0);
    }

    #[test]
    fn variable_tail_index_walk() {
        // CLMM: current_tick in the array [-55200,-54601], upward, 2 arrays → starts from a real tx.
        assert_eq!(clmm_tick_array_starts(-55000, 10, true, 2), vec![-55200, -54600]);
        assert_eq!(clmm_tick_array_starts(-55000, 10, false, 2), vec![-55200, -55800]);
        assert_eq!(clmm_tick_array_starts(0, 10, true, 3), vec![0, 600, 1200]);
        // DLMM: active_id -750 → index -11 (from a real tx); window in both directions.
        assert_eq!(dlmm_bin_array_indices(-750, true, 1), vec![-11]);
        assert_eq!(dlmm_bin_array_indices(-750, true, 2), vec![-11, -10]);
        assert_eq!(dlmm_bin_array_indices(-750, false, 2), vec![-11, -12]);
    }

    #[test]
    fn cpmm_whirlpool_pdas_match_real() {
        assert_eq!(cpmm_observation(&CPMM_POOL), CPMM_OBS);
        assert_eq!(whirlpool_oracle(&WP_POOL), WP_ORACLE);
    }

    #[test]
    fn manifest_pdas_match_real() {
        assert_eq!(manifest_global(&MF_QUOTEMINT), MF_GLOBAL);
        assert_eq!(manifest_global_vault(&MF_QUOTEMINT), MF_GLOBALVAULT);
    }

    #[test]
    fn pump_pda_matches_real() {
        assert_eq!(pump_user_volume_accumulator(&PUMP_USER), PUMP_UVA);
    }
}
