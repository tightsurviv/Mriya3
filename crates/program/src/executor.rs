//! On-chain route execution: per-leg account dispatcher + flow. The **pure part** (leg
//! account offset, account range of the i-th leg, amount chain model) is tested on the host. The **actual
//! SBF entrypoint** (reading token account balances, `sol_invoke_signed`) arrives with the build using the
//! Solana toolchain (platform-tools) — sketch in [`sbf_entrypoint_sketch`].
//!
//! Instruction account layout (as assembled by the off-chain `send::route` layer): `[7 fixed] +
//! [(n−1) intermediate triples] + [leg 0 slice] + [leg 1 slice] + …`, so the leg slices
//! start at offset `3·n + 4`, and the first account of each slice is the DEX program ID.

use crate::{hop, parse_header, ArbError};

/// Index of the bot's base token ATA in the prefix (BEFORE/AFTER balance is taken from this account).
pub const BASE_TOKEN_ACCOUNT_INDEX: usize = 2;

/// Offset of the first leg account = `3·n + 4` (7 fixed prefix accounts + (n−1) triples of intermediate accounts).
pub fn hop_accounts_offset(n_hops: u8) -> usize {
    3 * n_hops as usize + 4
}

/// Range `(offset, length)` of the i-th leg's accounts in the full instruction account list.
/// The first account of this range is the DEX program ID (CPI target).
pub fn hop_account_range(data: &[u8], i: u8) -> Result<(usize, usize), ArbError> {
    let h = parse_header(data)?;
    if i >= h.n_hops {
        return Err(ArbError::BadRoute);
    }
    let mut off = hop_accounts_offset(h.n_hops);
    let mut j = 0u8;
    while j < i {
        off += hop(data, j)?.n_accounts as usize;
        j += 1;
    }
    Ok((off, hop(data, i)?.n_accounts as usize))
}

/// Total number of route accounts per the descriptors: `Σ n_accounts + 3·n + 4` (account layout invariant).
pub fn total_accounts(data: &[u8]) -> Result<usize, ArbError> {
    let h = parse_header(data)?;
    let mut sum = hop_accounts_offset(h.n_hops);
    let mut j = 0u8;
    while j < h.n_hops {
        sum += hop(data, j)?.n_accounts as usize;
        j += 1;
    }
    Ok(sum)
}

/// Sketch of the SBF entrypoint (to be built with `solana-program`/`pinocchio` and the Solana toolchain):
///
/// ```text
/// #[cfg(target_os = "solana")]
/// entrypoint!(process_instruction);
/// fn process_instruction(program_id, accounts, data) -> ProgramResult {
///     // 1. Authorisation: accounts[0] = bot (signer); state PDA (accounts[1]) owner==program,
///     //    data[0]==1, authority==accounts[0]  (otherwise Custom(6902)).
///     let h = parse_header(data)?;                               // tag 0x0a, amount_in, min_profit(B), n w 2..=6
///     let base_ta = &accounts[BASE_TOKEN_ACCOUNT_INDEX];
///     let before = spl_token_amount(base_ta);                    // base balance BEFORE
///     let mut amount = h.amount_in;
///     for i in 0..h.n_hops {
///         let hp = hop(data, i)?;
///         let (off, len) = hop_account_range(data, i)?;
///         let hop_accounts = &accounts[off..off + len];
///         let dex_program = hop_accounts[0].key;                 // slice[0] = ID programu DEX
///         let mut ixdata = [0u8; 64];
///         let n = cpi::encode_swap(hp.venue, hp.direction, amount, 1, &mut ixdata)
///                    .ok_or(ProgramError::InvalidArgument)?;      // min_out = 1 (no leg slippage)
///         let metas = account_metas(&hop_accounts[1..]);          // flags from AccountInfo
///         sol_invoke_signed(&Instruction{program_id:*dex_program, accounts:metas, data:&ixdata[..n]},
///                           hop_accounts, &[&[b"state", &[bump]]])?;
///         amount = spl_token_amount(intermediate_ta_for(i));      // leg output = next leg input
///     }
///     let after = spl_token_amount(base_ta);                      // base balance AFTER
///     check_profit(before, after, h.min_profit)?;                 // profit ≤ threshold → revert (Custom)
///     Ok(())
/// }
/// ```
/// Note: `encode_swap` today handles venues with a known argument layout (CPMM, DAMM v2, CLMM,
/// DLMM). Whirlpool/Pump/Manifest need the instruction wired up and **on-chain validation** (devnet).
pub mod sbf_entrypoint_sketch {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_route, Hop, RouteMeta, HOP_LEN, HOPS_OFF};

    fn route(amount_in: u64, min_profit: u64, hops: &[(u8, u8, u8)]) -> Vec<u8> {
        let hops: Vec<Hop> = hops
            .iter()
            .map(|(v, dir, n)| Hop { venue: *v, direction: *dir, n_accounts: *n, token_account_size: 0 })
            .collect();
        encode_route(amount_in, min_profit, &hops, &RouteMeta::new(1, 0)).unwrap()
    }

    #[test]
    fn hop_offsets_match_invariant() {
        // 2 legs: Pump (28 accounts) + DLMM (18 accounts). Leg offset = 3·2+4 = 10.
        let data = route(1_000_000, 0, &[(0x05, 0, 28), (0x01, 0, 18)]);
        assert_eq!(hop_accounts_offset(2), 10);
        assert_eq!(hop_account_range(&data, 0).unwrap(), (10, 28));
        assert_eq!(hop_account_range(&data, 1).unwrap(), (38, 18));
        // Invariant: Σhop + 3n + 4 = 28 + 18 + 10 = 56.
        assert_eq!(total_accounts(&data).unwrap(), 56);
    }

    #[test]
    fn three_hop_ranges() {
        // 3 legs (15,17,15); offset = 3·3+4 = 13.
        let data = route(1, 0, &[(0x03, 0, 15), (0x02, 0, 17), (0x08, 1, 15)]);
        assert_eq!(hop_accounts_offset(3), 13);
        assert_eq!(hop_account_range(&data, 0).unwrap(), (13, 15));
        assert_eq!(hop_account_range(&data, 1).unwrap(), (28, 17));
        assert_eq!(hop_account_range(&data, 2).unwrap(), (45, 15));
        assert_eq!(total_accounts(&data).unwrap(), 60);
    }

    #[test]
    fn out_of_range_hop_errs() {
        let data = route(1, 0, &[(0x05, 0, 28), (0x01, 0, 18)]);
        assert_eq!(hop_account_range(&data, 2), Err(ArbError::BadRoute));
    }

    #[test]
    fn hops_start_at_62() {
        assert_eq!(HOPS_OFF, 62);
        assert_eq!(HOP_LEN, 5);
    }
}
