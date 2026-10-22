//! Assembling the account list of the arbitrage program instruction (`route_accounts`).
//!
//! The on-chain program reads accounts in a fixed order: a **fixed prefix** (bot accounts and sysvars),
//! one triple of accounts per intermediate token (token account, mint, token program), and then
//! per-leg account slices — each starting with the DEX program ID. Invariant
//! (11,174/11,174 real tx): `total_accounts = Σ hop.n_accounts + 3·n + 4` for `n` legs.
//!
//! The order and roles of the accounts were determined **empirically on real transactions of the reference bot**
//! (a set of real mainnet tx, outside the repo), not from memory of the ABI. Signer/writable flags follow the interface
//! of the given DEX program. This module is pure construction — no keys, signing or network.

use crate::tx::{AccountMeta, Pubkey};

/// System program (11111111111111111111111111111111).
pub const SYSTEM: Pubkey = [0u8; 32];
/// Program Memo `MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr` (distinguishing marker of the swap2/swap_v2 variants).
pub const MEMO: Pubkey = [5, 74, 83, 90, 153, 41, 33, 6, 77, 36, 232, 113, 96, 218, 56, 124, 124, 53, 181, 221, 188, 146, 187, 129, 228, 31, 168, 64, 65, 5, 68, 141];
/// SPL Token `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`.
pub const TOKEN: Pubkey = [6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169];
/// SPL Token-2022 `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`.
pub const TOKEN_2022: Pubkey = [6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77, 131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// A route must have at least 2 legs.
    TooFewHops,
    /// The number of intermediate account triples must be `n_hops − 1`.
    IntermediateCount,
    /// The declared `n_accounts` of the leg does not match the length of the assembled slice.
    HopLenMismatch { hop: usize, declared: u8, actual: usize },
}

fn m(pubkey: Pubkey, is_signer: bool, is_writable: bool) -> AccountMeta {
    AccountMeta { pubkey, is_signer, is_writable }
}

/// Fixed route prefix (7 accounts), verified on real tx:
/// `[0]` bot (signer, w) · `[1]` state PDA (w) · `[2]` bot's base token ATA (w) ·
/// `[3]` native-bridge PDA (w) · `[4]` WSOL mint · `[5]` System · `[6]` SysvarRent.
pub fn fixed_prefix(
    bot: Pubkey,
    state_pda: Pubkey,
    base_ata: Pubkey,
    native_bridge_pda: Pubkey,
    wsol_mint: Pubkey,
    rent_sysvar: Pubkey,
) -> [AccountMeta; 7] {
    [
        m(bot, true, true),
        m(state_pda, false, true),
        m(base_ata, false, true),
        m(native_bridge_pda, false, true),
        m(wsol_mint, false, false),
        m(SYSTEM, false, false),
        m(rent_sysvar, false, false),
    ]
}

/// Account triple for one intermediate token: token account (w), mint (ro), token program (ro).
pub fn intermediate_triple(token_account: Pubkey, mint: Pubkey, token_program: Pubkey) -> [AccountMeta; 3] {
    [m(token_account, false, true), m(mint, false, false), m(token_program, false, false)]
}

/// Assembles the full route account list: prefix + `n−1` intermediate triples + leg slices.
///
/// Checks the invariant `Σ hop + 3n + 4` and that each slice's length matches the declaration
/// `declared_n_accounts[i]` (the same `n_accounts` the route encoder writes into the instruction data).
pub fn assemble_route(
    prefix: &[AccountMeta; 7],
    intermediates: &[[AccountMeta; 3]],
    hops: &[Vec<AccountMeta>],
    declared_n_accounts: &[u8],
) -> Result<Vec<AccountMeta>, RouteError> {
    let n = hops.len();
    if n < 2 {
        return Err(RouteError::TooFewHops);
    }
    if intermediates.len() + 1 != n || declared_n_accounts.len() != n {
        return Err(RouteError::IntermediateCount);
    }
    for (i, (h, &declared)) in hops.iter().zip(declared_n_accounts).enumerate() {
        if h.len() != declared as usize {
            return Err(RouteError::HopLenMismatch { hop: i, declared, actual: h.len() });
        }
    }
    let sum_hops: usize = hops.iter().map(|h| h.len()).sum();
    let mut out = Vec::with_capacity(sum_hops + 3 * n + 4);
    out.extend_from_slice(prefix);
    for t in intermediates {
        out.extend_from_slice(t);
    }
    for h in hops {
        out.extend_from_slice(h);
    }
    debug_assert_eq!(out.len(), sum_hops + 3 * n + 4, "route account count invariant");
    Ok(out)
}

/// Pump AMM (PumpSwap) — account layout of one leg (venue 0x05). **28 accounts**, order and roles
/// verified on 7,341/7,341 real legs of the reference bot.
pub mod pump_amm {
    use super::{m, AccountMeta, Pubkey, SYSTEM};

    /// Pump AMM program ID `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` (pos. 0 and 17).
    pub const PROGRAM: Pubkey = [12, 20, 222, 252, 130, 94, 198, 118, 148, 37, 8, 24, 187, 101, 64, 101, 244, 41, 141, 49, 86, 213, 113, 180, 212, 248, 9, 12, 24, 233, 168, 99];
    /// Global config (pos. 3) — constant in 7,341/7,341 tx.
    pub const GLOBAL_CONFIG: Pubkey = [137, 11, 166, 68, 254, 31, 85, 170, 25, 241, 28, 210, 210, 236, 20, 211, 35, 59, 110, 10, 75, 234, 238, 247, 43, 105, 133, 142, 33, 225, 112, 214];
    /// Event authority (pos. 16) — constant.
    pub const EVENT_AUTHORITY: Pubkey = [229, 74, 112, 149, 40, 131, 159, 97, 192, 185, 184, 96, 121, 137, 28, 19, 146, 22, 228, 122, 113, 182, 47, 183, 59, 236, 114, 22, 148, 88, 116, 94];
    /// Global volume accumulator (pos. 20) — constant.
    pub const GLOBAL_VOLUME_ACCUMULATOR: Pubkey = [163, 215, 187, 18, 126, 88, 173, 193, 44, 166, 143, 131, 67, 126, 194, 225, 195, 249, 130, 13, 233, 62, 88, 249, 23, 138, 41, 24, 221, 170, 247, 180];
    /// Fee config (pos. 22) — constant.
    pub const FEE_CONFIG: Pubkey = [65, 36, 110, 204, 125, 120, 254, 129, 228, 23, 115, 164, 105, 101, 65, 153, 55, 146, 58, 7, 100, 71, 151, 223, 111, 62, 181, 20, 66, 96, 16, 203];
    /// Fee program `pfee…` (pos. 23) — constant.
    pub const FEE_PROGRAM: Pubkey = [12, 53, 255, 169, 5, 90, 142, 86, 141, 168, 247, 188, 7, 86, 21, 39, 76, 241, 201, 44, 164, 31, 64, 0, 156, 81, 106, 164, 20, 194, 124, 112];
    /// Associated token account program (ATA, pos. 15) — constant.
    pub const ATA_PROGRAM: Pubkey = [140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89];

    /// Inputs of one Pump AMM leg. Fields from the pool decoder and the route prefix + accounts resolved
    /// live by the `feed` layer (fee recipient ATA, creator vault PDA, user accumulator,
    /// 4 derived accounts per pool — pos. 24..27). Program constants are hardcoded above.
    #[derive(Clone, Debug)]
    pub struct PumpAmmHop {
        /// [1] pool account.
        pub pool: Pubkey,
        /// [4] base mint (from the pool decoder).
        pub base_mint: Pubkey,
        /// [5] quote mint (from the pool decoder; usually WSOL).
        pub quote_mint: Pubkey,
        /// [8] base vault (from the pool decoder).
        pub base_vault: Pubkey,
        /// [9] quote vault (from the pool decoder).
        pub quote_vault: Pubkey,
        /// [2] user = bot (signer).
        pub user: Pubkey,
        /// [6] bot ATA for base_mint (= intermediate account from the prefix).
        pub user_base_ata: Pubkey,
        /// [7] bot ATA for quote_mint (= base ATA from the prefix).
        pub user_quote_ata: Pubkey,
        /// [10] protocol fee recipient (one of a set of 8).
        pub fee_recipient: Pubkey,
        /// [11] fee recipient ATA for quote_mint.
        pub fee_recipient_ata: Pubkey,
        /// [12] base token program (Token or Token-2022).
        pub base_token_program: Pubkey,
        /// [13] quote token program.
        pub quote_token_program: Pubkey,
        /// [18] coin creator vault ATA (PDA per pool).
        pub creator_vault_ata: Pubkey,
        /// [19] creator vault authority (PDA per pool).
        pub creator_vault_authority: Pubkey,
        /// [21] user volume accumulator (constant for a given bot).
        pub user_volume_accumulator: Pubkey,
        /// [24..27] 4 derived accounts per pool/creator (roles resolved separately; flags set by the resolver).
        pub extra: [AccountMeta; 4],
    }

    impl PumpAmmHop {
        /// Number of leg accounts (constant for Pump AMM).
        pub const N_ACCOUNTS: u8 = 28;

        /// Builds the 28 leg accounts in the verified order. Signer/writable flags per the PumpSwap
        /// interface (writable: pool, user ATA, vaults, fee ATA, creator vault, accumulators;
        /// signer: user only). Pos. 24..27 with flags given in `extra`.
        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = Vec::with_capacity(Self::N_ACCOUNTS as usize);
            v.push(m(PROGRAM, false, false)); // 0
            v.push(m(self.pool, false, true)); // 1
            v.push(m(self.user, true, true)); // 2
            v.push(m(GLOBAL_CONFIG, false, false)); // 3
            v.push(m(self.base_mint, false, false)); // 4
            v.push(m(self.quote_mint, false, false)); // 5
            v.push(m(self.user_base_ata, false, true)); // 6
            v.push(m(self.user_quote_ata, false, true)); // 7
            v.push(m(self.base_vault, false, true)); // 8
            v.push(m(self.quote_vault, false, true)); // 9
            v.push(m(self.fee_recipient, false, false)); // 10
            v.push(m(self.fee_recipient_ata, false, true)); // 11
            v.push(m(self.base_token_program, false, false)); // 12
            v.push(m(self.quote_token_program, false, false)); // 13
            v.push(m(SYSTEM, false, false)); // 14
            v.push(m(ATA_PROGRAM, false, false)); // 15
            v.push(m(EVENT_AUTHORITY, false, false)); // 16
            v.push(m(PROGRAM, false, false)); // 17
            v.push(m(self.creator_vault_ata, false, true)); // 18
            v.push(m(self.creator_vault_authority, false, false)); // 19
            v.push(m(GLOBAL_VOLUME_ACCUMULATOR, false, true)); // 20
            v.push(m(self.user_volume_accumulator, false, true)); // 21
            v.push(m(FEE_CONFIG, false, false)); // 22
            v.push(m(FEE_PROGRAM, false, false)); // 23
            v.push(self.extra[0].clone()); // 24
            v.push(self.extra[1].clone()); // 25
            v.push(self.extra[2].clone()); // 26
            v.push(self.extra[3].clone()); // 27
            v
        }
    }
}

/// Meteora DLMM `swap2` (venue 0x01). **18+ accounts**: core 0..16 + variable BinArray tail (1–2,
/// writable, per-pool PDA). Order/roles verified on 6,406/6,406 real legs.
pub mod dlmm {
    use super::{m, AccountMeta, Pubkey, MEMO};

    /// Program DLMM `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` (pos. 0, 16; sentinel None for pos. 2/10).
    pub const PROGRAM: Pubkey = [4, 233, 225, 47, 188, 132, 232, 38, 201, 50, 204, 233, 226, 100, 12, 206, 21, 89, 12, 28, 98, 115, 176, 146, 87, 8, 186, 59, 133, 32, 176, 188];
    /// DLMM event authority (pos. 15) — constant.
    pub const EVENT_AUTHORITY: Pubkey = [178, 112, 214, 127, 169, 140, 81, 207, 2, 19, 5, 19, 88, 150, 43, 175, 53, 116, 43, 237, 89, 201, 217, 68, 94, 156, 13, 12, 133, 199, 205, 145];

    /// Inputs of a DLMM leg. `bitmap_extension`/`host_fee_in` = `PROGRAM` when None (sentinel).
    #[derive(Clone, Debug)]
    pub struct DlmmHop {
        pub lb_pair: Pubkey,
        pub bitmap_extension: Pubkey,
        pub reserve_x: Pubkey,
        pub reserve_y: Pubkey,
        pub user_token_in: Pubkey,
        pub user_token_out: Pubkey,
        pub token_x_mint: Pubkey,
        pub token_y_mint: Pubkey,
        pub oracle: Pubkey,
        pub host_fee_in: Pubkey,
        pub user: Pubkey,
        pub token_x_program: Pubkey,
        pub token_y_program: Pubkey,
        /// Variable tail: 1–2 BinArray accounts (writable, per-pool PDA; supplied by `feed` from the pool state).
        pub bin_arrays: Vec<Pubkey>,
    }

    impl DlmmHop {
        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = Vec::with_capacity(17 + self.bin_arrays.len());
            v.push(m(PROGRAM, false, false));
            v.push(m(self.lb_pair, false, true));
            v.push(m(self.bitmap_extension, false, false));
            v.push(m(self.reserve_x, false, true));
            v.push(m(self.reserve_y, false, true));
            v.push(m(self.user_token_in, false, true));
            v.push(m(self.user_token_out, false, true));
            v.push(m(self.token_x_mint, false, false));
            v.push(m(self.token_y_mint, false, false));
            v.push(m(self.oracle, false, true));
            v.push(m(self.host_fee_in, false, false));
            v.push(m(self.user, true, false));
            v.push(m(self.token_x_program, false, false));
            v.push(m(self.token_y_program, false, false));
            v.push(m(MEMO, false, false));
            v.push(m(EVENT_AUTHORITY, false, false));
            v.push(m(PROGRAM, false, false));
            for ba in &self.bin_arrays {
                v.push(m(*ba, false, true));
            }
            v
        }

        /// `swap` (venue 0x23). Same layout as [`Self::accounts`], without the MEMO account.
        /// Window measurement: 1907 legs with 17 accounts and 29 with 18 (second BinArray). Both token programs are SPL.
        pub fn accounts_v1(&self) -> Vec<AccountMeta> {
            let mut v = self.accounts();
            v.remove(14);
            v
        }
    }
}

/// Raydium CPMM `swap_base_input` (venue 0x03). **15 accounts**, fixed length.
pub mod cpmm {
    use super::{m, AccountMeta, Pubkey};

    /// Program CPMM `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` (pos. 0, 14).
    pub const PROGRAM: Pubkey = [169, 42, 90, 139, 79, 41, 89, 82, 132, 37, 80, 170, 147, 253, 91, 149, 181, 172, 230, 168, 235, 146, 12, 147, 148, 46, 67, 105, 12, 32, 236, 115];
    /// Vault/LP authority (pos. 2) — program PDA constant.
    pub const AUTHORITY: Pubkey = [235, 0, 217, 245, 178, 146, 180, 33, 74, 199, 208, 55, 180, 214, 240, 100, 80, 185, 100, 96, 13, 243, 115, 5, 43, 181, 232, 79, 47, 142, 154, 103];

    #[derive(Clone, Debug)]
    pub struct CpmmHop {
        pub payer: Pubkey,
        pub amm_config: Pubkey,
        pub pool_state: Pubkey,
        pub input_token_account: Pubkey,
        pub output_token_account: Pubkey,
        pub input_vault: Pubkey,
        pub output_vault: Pubkey,
        pub input_token_program: Pubkey,
        pub output_token_program: Pubkey,
        pub input_token_mint: Pubkey,
        pub output_token_mint: Pubkey,
        pub observation_state: Pubkey,
    }

    impl CpmmHop {
        pub const N_ACCOUNTS: u8 = 15;
        pub fn accounts(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(self.payer, true, false),
                m(AUTHORITY, false, false),
                m(self.amm_config, false, false),
                m(self.pool_state, false, true),
                m(self.input_token_account, false, true),
                m(self.output_token_account, false, true),
                m(self.input_vault, false, true),
                m(self.output_vault, false, true),
                m(self.input_token_program, false, false),
                m(self.output_token_program, false, false),
                m(self.input_token_mint, false, false),
                m(self.output_token_mint, false, false),
                m(self.observation_state, false, true),
                m(PROGRAM, false, false),
            ]
        }
    }
}

/// Orca Whirlpool `swap_v2` (venue 0x04). **18+ accounts**: core 0..15 (3 tick_array + oracle) +
/// variable tail of additional tick_array (writable, per-pool PDA).
pub mod whirlpool {
    use super::{m, AccountMeta, Pubkey, MEMO};

    /// Program Whirlpool `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` (pos. 0).
    pub const PROGRAM: Pubkey = [14, 3, 104, 95, 142, 144, 144, 83, 228, 88, 18, 28, 102, 245, 167, 106, 237, 199, 112, 106, 161, 28, 130, 248, 170, 149, 42, 143, 43, 120, 121, 169];

    #[derive(Clone, Debug)]
    pub struct WhirlpoolHop {
        pub token_program_a: Pubkey,
        pub token_program_b: Pubkey,
        pub token_authority: Pubkey,
        pub whirlpool: Pubkey,
        pub token_mint_a: Pubkey,
        pub token_mint_b: Pubkey,
        pub token_owner_a: Pubkey,
        pub token_vault_a: Pubkey,
        pub token_owner_b: Pubkey,
        pub token_vault_b: Pubkey,
        pub tick_array_0: Pubkey,
        pub tick_array_1: Pubkey,
        pub tick_array_2: Pubkey,
        pub oracle: Pubkey,
        /// Additional tick_array (remaining accounts, writable) — 0..N, depending on the swap range.
        pub supplemental_tick_arrays: Vec<Pubkey>,
    }

    impl WhirlpoolHop {
        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = Vec::with_capacity(16 + self.supplemental_tick_arrays.len());
            v.push(m(PROGRAM, false, false));
            v.push(m(self.token_program_a, false, false));
            v.push(m(self.token_program_b, false, false));
            v.push(m(MEMO, false, false));
            v.push(m(self.token_authority, true, false));
            v.push(m(self.whirlpool, false, true));
            v.push(m(self.token_mint_a, false, false));
            v.push(m(self.token_mint_b, false, false));
            v.push(m(self.token_owner_a, false, true));
            v.push(m(self.token_vault_a, false, true));
            v.push(m(self.token_owner_b, false, true));
            v.push(m(self.token_vault_b, false, true));
            v.push(m(self.tick_array_0, false, true));
            v.push(m(self.tick_array_1, false, true));
            v.push(m(self.tick_array_2, false, true));
            v.push(m(self.oracle, false, true));
            for ta in &self.supplemental_tick_arrays {
                v.push(m(*ta, false, true));
            }
            v
        }

        /// Classic `swap` (venue 0x25). 12 accounts: one SPL program, no MEMO, mints or tail.
        /// Measurement: 841/841 legs have width 12.
        pub fn accounts_v1(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(self.token_program_a, false, false),
                m(self.token_authority, true, false),
                m(self.whirlpool, false, true),
                m(self.token_owner_a, false, true),
                m(self.token_vault_a, false, true),
                m(self.token_owner_b, false, true),
                m(self.token_vault_b, false, true),
                m(self.tick_array_0, false, true),
                m(self.tick_array_1, false, true),
                m(self.tick_array_2, false, true),
                m(self.oracle, false, true),
            ]
        }
    }
}

/// Meteora DAMM v2 `swap` (venue 0x08). **15 accounts**, fixed length.
pub mod damm_v2 {
    use super::{m, AccountMeta, Pubkey};

    /// Program DAMM v2 `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` (pos. 0, 12, 14).
    pub const PROGRAM: Pubkey = [9, 45, 33, 53, 101, 122, 21, 156, 43, 135, 212, 182, 106, 112, 219, 142, 151, 82, 56, 159, 247, 106, 175, 32, 108, 237, 6, 58, 56, 249, 90, 237];
    /// Pool authority (pos. 1) — program PDA constant.
    pub const POOL_AUTHORITY: Pubkey = [242, 204, 213, 53, 172, 165, 241, 115, 106, 200, 34, 221, 7, 115, 228, 217, 47, 189, 138, 89, 178, 148, 3, 80, 2, 149, 169, 1, 28, 115, 169, 229];
    /// Event authority (pos. 13) — constant.
    pub const EVENT_AUTHORITY: Pubkey = [42, 118, 231, 179, 68, 100, 10, 28, 252, 89, 76, 139, 202, 208, 160, 145, 1, 28, 172, 125, 209, 86, 191, 131, 168, 51, 251, 34, 8, 235, 119, 173];

    /// `referral_token_account` = `PROGRAM` when None (Anchor optional sentinel).
    #[derive(Clone, Debug)]
    pub struct DammV2Hop {
        pub pool: Pubkey,
        pub input_token_account: Pubkey,
        pub output_token_account: Pubkey,
        pub token_a_vault: Pubkey,
        pub token_b_vault: Pubkey,
        pub token_a_mint: Pubkey,
        pub token_b_mint: Pubkey,
        pub payer: Pubkey,
        pub token_a_program: Pubkey,
        pub token_b_program: Pubkey,
        pub referral_token_account: Pubkey,
    }

    impl DammV2Hop {
        pub const N_ACCOUNTS: u8 = 15;
        pub fn accounts(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(POOL_AUTHORITY, false, false),
                m(self.pool, false, true),
                m(self.input_token_account, false, true),
                m(self.output_token_account, false, true),
                m(self.token_a_vault, false, true),
                m(self.token_b_vault, false, true),
                m(self.token_a_mint, false, false),
                m(self.token_b_mint, false, false),
                m(self.payer, true, false),
                m(self.token_a_program, false, false),
                m(self.token_b_program, false, false),
                m(self.referral_token_account, false, false),
                m(EVENT_AUTHORITY, false, false),
                m(PROGRAM, false, false),
            ]
        }
    }
}

/// Manifest `swap` (venue 0x10).
///
/// Window: 2872 legs with 15 accounts each (different programs; always Token-2022 on base and SPL on quote),
/// 223 legs with 13 accounts (both SPL Token: one program and the quote mint, no base mint)
/// and 1 leg with 14 accounts (both Token-2022: one program and both mints).
/// `global` / `global_vault` stay derived from the quote mint.
pub mod manifest {
    use super::{m, AccountMeta, Pubkey, SYSTEM, TOKEN, TOKEN_2022};

    /// Program Manifest `MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms` (pos. 0).
    pub const PROGRAM: Pubkey = [5, 55, 164, 119, 56, 218, 37, 169, 136, 249, 100, 217, 155, 164, 58, 136, 207, 17, 135, 57, 91, 106, 207, 20, 131, 245, 112, 34, 141, 128, 2, 178];

    #[derive(Clone, Debug)]
    pub struct ManifestHop {
        pub payer: Pubkey,
        pub owner: Pubkey,
        pub market: Pubkey,
        pub trader_base: Pubkey,
        pub trader_quote: Pubkey,
        pub base_vault: Pubkey,
        pub quote_vault: Pubkey,
        pub token_program_base: Pubkey,
        pub base_mint: Pubkey,
        pub token_program_quote: Pubkey,
        pub quote_mint: Pubkey,
        /// `global` and `global_vault` — per-mint PDA (writable; supplied by `feed`).
        pub global: Pubkey,
        pub global_vault: Pubkey,
    }

    impl ManifestHop {
        /// Modal list when the token programs differ.
        pub const N_ACCOUNTS: u8 = 15;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = Vec::with_capacity(Self::N_ACCOUNTS as usize);
            v.push(m(PROGRAM, false, false));
            v.push(m(self.payer, true, true));
            v.push(m(self.owner, true, false));
            v.push(m(self.market, false, true));
            v.push(m(SYSTEM, false, false));
            v.push(m(self.trader_base, false, true));
            v.push(m(self.trader_quote, false, true));
            v.push(m(self.base_vault, false, true));
            v.push(m(self.quote_vault, false, true));
            v.push(m(self.token_program_base, false, false));
            let same = self.token_program_base == self.token_program_quote;
            if same && self.token_program_base == TOKEN {
                // 13: no second program and no base mint. The mint at this position is quote.
                v.push(m(self.quote_mint, false, false));
            } else if same && self.token_program_base == TOKEN_2022 {
                // 14: no second program. Both mints stay, base before quote.
                v.push(m(self.base_mint, false, false));
                v.push(m(self.quote_mint, false, false));
            } else {
                v.push(m(self.base_mint, false, false));
                v.push(m(self.token_program_quote, false, false));
                v.push(m(self.quote_mint, false, false));
            }
            v.push(m(self.global, false, true));
            v.push(m(self.global_vault, false, true));
            v
        }
    }
}

/// Raydium CLMM `swap_v2` (venue 0x02). **14+ accounts**: core 0..13 + variable tail of tick_array
/// (writable, per-pool PDA; count depends on pool state, direction and input size).
pub mod clmm {
    use super::{m, AccountMeta, Pubkey, MEMO, TOKEN, TOKEN_2022};

    /// Program Raydium CLMM `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` (pos. 0).
    pub const PROGRAM: Pubkey = [165, 213, 202, 158, 4, 207, 93, 181, 144, 183, 20, 186, 47, 227, 44, 177, 89, 19, 63, 193, 193, 146, 183, 34, 87, 253, 7, 211, 156, 176, 64, 30];

    #[derive(Clone, Debug)]
    pub struct ClmmHop {
        /// Raydium `CAMMCzo`, Pancake `HpNfyc` or Byreal `REALQq`.
        pub program: Pubkey,
        pub payer: Pubkey,
        pub amm_config: Pubkey,
        pub pool_state: Pubkey,
        pub input_token_account: Pubkey,
        pub output_token_account: Pubkey,
        pub input_vault: Pubkey,
        pub output_vault: Pubkey,
        pub observation_state: Pubkey,
        pub input_vault_mint: Pubkey,
        pub output_vault_mint: Pubkey,
        /// Variable tail: 1..N tick_array (writable, per-pool PDA; supplied by `feed` from the pool state).
        pub tick_arrays: Vec<Pubkey>,
    }

    impl ClmmHop {
        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = Vec::with_capacity(14 + self.tick_arrays.len());
            v.push(m(self.program, false, false));
            v.push(m(self.payer, true, false));
            v.push(m(self.amm_config, false, false));
            v.push(m(self.pool_state, false, true));
            v.push(m(self.input_token_account, false, true));
            v.push(m(self.output_token_account, false, true));
            v.push(m(self.input_vault, false, true));
            v.push(m(self.output_vault, false, true));
            v.push(m(self.observation_state, false, true));
            v.push(m(TOKEN, false, false));
            v.push(m(TOKEN_2022, false, false));
            v.push(m(MEMO, false, false));
            v.push(m(self.input_vault_mint, false, false));
            v.push(m(self.output_vault_mint, false, false));
            for ta in &self.tick_arrays {
                v.push(m(*ta, false, true));
            }
            v
        }

        /// `swap` (venue 0x24). No Token-2022, MEMO or both mints.
        /// Modally 13 accounts = 10 fixed (with the program) + 3 tick arrays.
        pub fn accounts_v1(&self) -> Vec<AccountMeta> {
            let v2 = self.accounts();
            let mut v = Vec::with_capacity(10 + self.tick_arrays.len());
            v.extend_from_slice(&v2[..10]);
            v.extend_from_slice(&v2[14..]);
            v
        }
    }
}

/// Raydium AMM v4 `SwapBaseInV2` (venue 0x26, instruction tag `0x10`). **9 accounts**.
/// 245/245 aligned legs have this width. The order is the V2 list without the OpenBook accounts:
/// program, SPL Token, pool, authority, coin vault, pc vault, source, destination, owner.
pub mod ray_v4 {
    use super::{m, AccountMeta, Pubkey, TOKEN};

    /// AMM v4 program `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`.
    pub const PROGRAM: Pubkey = [75, 217, 73, 196, 54, 2, 195, 63, 32, 119, 144, 237, 22, 163, 82, 76, 161, 185, 151, 92, 241, 33, 162, 169, 12, 255, 236, 125, 248, 182, 138, 205];
    /// AMM authority `5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1`. On every leg in the window.
    pub const AUTHORITY: Pubkey = [65, 87, 176, 88, 15, 49, 197, 252, 228, 74, 98, 88, 45, 188, 249, 215, 142, 231, 89, 67, 160, 132, 163, 147, 179, 80, 54, 141, 34, 137, 147, 8];

    #[derive(Clone, Debug)]
    pub struct RayV4Hop {
        pub amm: Pubkey,
        pub coin_vault: Pubkey,
        pub pc_vault: Pubkey,
        pub user_source: Pubkey,
        pub user_destination: Pubkey,
        pub user_owner: Pubkey,
    }

    impl RayV4Hop {
        pub const N_ACCOUNTS: u8 = 9;
        pub fn accounts(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(TOKEN, false, false),
                m(self.amm, false, true),
                m(AUTHORITY, false, false),
                m(self.coin_vault, false, true),
                m(self.pc_vault, false, true),
                m(self.user_source, false, true),
                m(self.user_destination, false, true),
                m(self.user_owner, true, false),
            ]
        }
    }
}

/// Meteora DAMM v1 `swap` (venue 0x00). **16 accounts**: program and 15 CPI accounts.
/// The order is constant across the 73 legs of the window. Accounts 1 and 2 swap with the direction
/// (source, then destination). Vaults, LP mints and LP shares stay with the pool.
/// The protocol fee account differs for each side.
pub mod damm_v1 {
    use super::{m, AccountMeta, Pubkey, TOKEN};

    /// Dynamic AMM program `Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB`.
    pub const PROGRAM: Pubkey = [204, 248, 2, 212, 204, 204, 132, 215, 251, 33, 181, 247, 59, 73, 216, 26, 22, 197, 180, 200, 142, 227, 35, 148, 225, 201, 29, 53, 136, 204, 64, 128];
    /// Vault program `24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi`. On every leg.
    pub const VAULT_PROGRAM: Pubkey = [15, 191, 232, 132, 109, 104, 92, 189, 198, 44, 202, 126, 4, 199, 232, 246, 141, 204, 49, 58, 179, 18, 119, 226, 224, 17, 42, 46, 192, 224, 82, 229];

    #[derive(Clone, Debug)]
    pub struct DammV1Hop {
        pub pool: Pubkey,
        pub user_source: Pubkey,
        pub user_destination: Pubkey,
        pub a_vault: Pubkey,
        pub b_vault: Pubkey,
        pub a_token_vault: Pubkey,
        pub b_token_vault: Pubkey,
        pub a_lp_mint: Pubkey,
        pub b_lp_mint: Pubkey,
        pub a_vault_lp: Pubkey,
        pub b_vault_lp: Pubkey,
        pub protocol_fee: Pubkey,
        pub user: Pubkey,
    }

    impl DammV1Hop {
        pub const N_ACCOUNTS: u8 = 16;
        pub fn accounts(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(self.pool, false, true),
                m(self.user_source, false, true),
                m(self.user_destination, false, true),
                m(self.a_vault, false, true),
                m(self.b_vault, false, true),
                m(self.a_token_vault, false, true),
                m(self.b_token_vault, false, true),
                m(self.a_lp_mint, false, false),
                m(self.b_lp_mint, false, false),
                m(self.a_vault_lp, false, true),
                m(self.b_vault_lp, false, true),
                m(self.protocol_fee, false, true),
                m(self.user, true, false),
                m(VAULT_PROGRAM, false, false),
                m(TOKEN, false, false),
            ]
        }
    }
}

/// Futarchy `spot_swap` (venue 0x16). **10 accounts**.
///
/// 406/406 legs of tag 0x0a have this width. The order is the IDL `spotSwap` list
/// after the program: dao, user base account, quote account, both vaults, user,
/// SPL Token, event authority, program. No curve, so the planner does not pick this leg.
pub mod futarchy {
    use super::{m, AccountMeta, Pubkey, TOKEN};

    /// Program `FUTARELBfJfQ8RDGhg1wdhddq1odMAJUePHFuBYfUxKq` (pos. 0 and 9).
    pub const PROGRAM: Pubkey = [215, 11, 225, 117, 239, 90, 200, 156, 79, 12, 238, 230, 4, 174, 229, 152, 18, 91, 34, 223, 74, 202, 43, 145, 5, 197, 103, 227, 54, 18, 102, 120];
    /// Event authority `DGEympSS4qLvdr9r3uGHTfACdN8snShk4iGdJtZPxuBC`. 406/406, read-only.
    pub const EVENT_AUTHORITY: Pubkey = [182, 51, 194, 220, 129, 250, 165, 156, 5, 248, 7, 79, 115, 30, 11, 255, 172, 40, 173, 19, 179, 162, 18, 218, 30, 52, 5, 3, 226, 126, 230, 215];

    #[derive(Clone, Debug)]
    pub struct FutarchyHop {
        pub dao: Pubkey,
        pub user_base: Pubkey,
        pub user_quote: Pubkey,
        pub amm_base_vault: Pubkey,
        pub amm_quote_vault: Pubkey,
        pub user: Pubkey,
    }

    impl FutarchyHop {
        pub const N_ACCOUNTS: u8 = 10;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            vec![
                m(PROGRAM, false, false),
                m(self.dao, false, true),
                m(self.user_base, false, true),
                m(self.user_quote, false, true),
                m(self.amm_base_vault, false, true),
                m(self.amm_quote_vault, false, true),
                m(self.user, true, false),
                m(TOKEN, false, false),
                m(EVENT_AUTHORITY, false, false),
                m(PROGRAM, false, false),
            ]
        }
    }
}

/// DFlow Aggregator v4 `swap2` with a BisonFi step (venue 0x17). **19 accounts**.
///
/// 914/914 legs of tag 0x0a. The route direction is always 0. Positions 0..12 and 18 are constant.
/// The five accounts 13..17 depend on the market and are writable. 16 and 17 repeat the route's
/// token accounts (914/914). No curve, so the planner does not pick this leg.
pub mod dflow {
    use super::{m, AccountMeta, Pubkey, SYSTEM, TOKEN, TOKEN_2022};

    /// Program `DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH`.
    pub const PROGRAM: Pubkey = [181, 227, 74, 20, 226, 188, 115, 72, 105, 14, 225, 245, 175, 93, 238, 214, 85, 56, 64, 163, 109, 170, 184, 96, 176, 80, 96, 115, 189, 192, 60, 16];
    /// Event authority `8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d`. 914/914, read-only.
    pub const EVENT_AUTHORITY: Pubkey = [118, 67, 61, 254, 126, 238, 253, 42, 29, 49, 254, 10, 29, 68, 229, 70, 53, 8, 52, 153, 130, 154, 89, 218, 42, 21, 152, 40, 19, 198, 222, 120];
    /// Program BisonFi `BiSoNHVpsVZW2F7rx2eQ59yQwKxzU5NvBcmKshCSUypi`. 914/914.
    pub const BISON: Pubkey = [159, 50, 218, 156, 31, 108, 18, 80, 192, 155, 115, 57, 35, 236, 195, 103, 13, 40, 99, 231, 212, 106, 156, 28, 244, 125, 253, 187, 208, 213, 254, 87];
    /// Sysvar Instructions. At position 7 of every 0x17 leg.
    pub const SYSVAR_INSTRUCTIONS: Pubkey = [6, 167, 213, 23, 24, 123, 209, 102, 53, 218, 212, 4, 85, 253, 194, 192, 193, 36, 198, 143, 33, 86, 117, 165, 219, 186, 203, 95, 8, 0, 0, 0];
    /// Program ATA `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`.
    pub const ATA_PROGRAM: Pubkey = [140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89];

    #[derive(Clone, Debug)]
    pub struct DflowHop {
        pub user: Pubkey,
        /// Positions 13..17. The caller supplies the market keys. The flags are writable.
        pub market: [Pubkey; 5],
    }

    impl DflowHop {
        pub const N_ACCOUNTS: u8 = 19;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut v = vec![
                m(PROGRAM, false, false),
                m(TOKEN, false, false),
                m(ATA_PROGRAM, false, false),
                m(SYSTEM, false, false),
                m(self.user, true, false),
                m(EVENT_AUTHORITY, false, false),
                m(PROGRAM, false, false),
                m(SYSVAR_INSTRUCTIONS, false, false),
                m(TOKEN_2022, false, false),
                m(PROGRAM, false, false),
                m(PROGRAM, false, false),
                m(BISON, false, false),
                m(self.user, true, false),
            ];
            for pk in self.market {
                v.push(m(pk, false, true));
            }
            v.push(m(TOKEN, false, false));
            v
        }
    }
}

fn metas(keys: &[Pubkey], flags: &[(bool, bool)]) -> Vec<AccountMeta> {
    assert_eq!(keys.len(), flags.len());
    keys.iter().zip(flags).map(|(&pubkey, &(is_signer, is_writable))| m(pubkey, is_signer, is_writable)).collect()
}
/// Constants measured on legs from the window of real tx (2026-09-26). The user is an argument, not an address from the sample.
mod measured_keys {
    use super::Pubkey;
    /// `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`. Position 0 and last.
    pub const PUMP_FUN_PROGRAM: Pubkey = [1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151, 245, 210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176];
    /// `4wTV1Ymi…`. Position 1, 22/22 and 11/11.
    pub const PUMP_GLOBAL: Pubkey = [58, 134, 94, 105, 238, 15, 84, 128, 202, 188, 246, 99, 87, 228, 220, 47, 24, 213, 141, 69, 193, 234, 116, 137, 251, 55, 35, 217, 121, 60, 114, 166];
    /// `62qc2CNX…`. Position 7, write, 22/22 and 11/11.
    pub const PUMP_SLOT7: Pubkey = [74, 194, 248, 208, 221, 92, 188, 151, 227, 40, 156, 25, 124, 181, 6, 42, 84, 243, 217, 86, 185, 206, 110, 81, 21, 249, 101, 103, 170, 92, 179, 230];
    /// `5YxQFdt3…`. Position 9, write, 22/22 and 11/11.
    pub const PUMP_SLOT9: Pubkey = [67, 158, 101, 16, 192, 61, 101, 250, 217, 49, 232, 157, 4, 190, 11, 183, 13, 81, 151, 31, 81, 196, 21, 251, 52, 76, 7, 219, 65, 159, 33, 34];
    /// `94qWNrtm…`. Position 8 for `sell_v2`, write, 11/11.
    pub const PUMP_SLOT94: Pubkey = [119, 217, 21, 149, 95, 136, 128, 115, 28, 235, 74, 117, 160, 204, 150, 193, 116, 250, 64, 149, 196, 225, 217, 150, 122, 207, 196, 40, 69, 174, 103, 174];
    /// `HjQjngTD…`. Position 10 for `sell_v2`, write, 11/11.
    pub const PUMP_SLOT_HJ: Pubkey = [248, 152, 57, 161, 203, 125, 233, 89, 93, 205, 119, 209, 194, 133, 213, 106, 118, 200, 245, 43, 125, 228, 46, 79, 31, 120, 218, 130, 31, 108, 74, 219];
    /// `Hq2wp8uJ…`. Position 20 for `buy_exact_quote_in_v2`, 22/22.
    pub const PUMP_HQ: Pubkey = [250, 9, 17, 165, 72, 99, 65, 45, 99, 31, 78, 7, 135, 3, 41, 108, 3, 95, 13, 19, 51, 160, 217, 200, 131, 141, 115, 183, 16, 254, 110, 45];
    /// `5U4J3QX2…`. Write. Position 21 for buy, 20 for sell.
    pub const PUMP_U4: Pubkey = [66, 93, 39, 100, 222, 170, 206, 253, 13, 6, 138, 135, 201, 40, 49, 198, 168, 47, 157, 192, 61, 240, 159, 241, 178, 112, 129, 250, 178, 252, 133, 27];
    /// `3WxapqAy…`. Write for `sell_v2`, position 21, 11/11.
    pub const PUMP_WX: Pubkey = [37, 100, 16, 191, 131, 53, 2, 237, 173, 170, 202, 72, 205, 231, 123, 190, 219, 119, 15, 122, 3, 248, 1, 6, 205, 111, 243, 140, 176, 157, 99, 146];
    /// `8Wf5TiAh…`. Position 23 for buy and 22 for sell.
    pub const PUMP_WF: Pubkey = [111, 154, 180, 164, 241, 149, 141, 192, 169, 201, 76, 63, 183, 44, 7, 153, 88, 67, 237, 164, 133, 227, 162, 79, 16, 198, 147, 153, 248, 25, 148, 15];
    /// `pfeeUxB6…`. Fee program.
    pub const PUMP_FEE_PROGRAM: Pubkey = [12, 53, 255, 169, 5, 90, 142, 86, 141, 168, 247, 188, 7, 86, 21, 39, 76, 241, 201, 44, 164, 31, 64, 0, 156, 81, 106, 164, 20, 194, 124, 112];
    /// `Ce6TQqeH…`. Event authority.
    pub const PUMP_EVENT: Pubkey = [172, 241, 54, 235, 1, 252, 28, 78, 136, 61, 35, 200, 181, 132, 74, 181, 154, 55, 246, 106, 221, 87, 197, 233, 172, 59, 83, 224, 89, 211, 92, 100];
    /// WSOL. For `sell_v2` position 3, 11/11.
    pub const PUMP_WSOL: Pubkey = [6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26, 235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1];
    /// `LanMV9sAd7…`.
    pub const LAN_PROGRAM: Pubkey = [5, 4, 59, 149, 77, 202, 38, 225, 239, 145, 181, 44, 79, 143, 137, 175, 138, 111, 90, 200, 198, 33, 86, 241, 113, 207, 15, 33, 172, 81, 201, 34];
    /// `WLHv2UAZ…`. Position 2, 11/11 and 13/13.
    pub const LAN_AUTH: Pubkey = [7, 131, 168, 21, 38, 33, 15, 148, 160, 232, 232, 77, 32, 135, 0, 59, 58, 66, 96, 210, 156, 231, 157, 172, 254, 33, 164, 234, 119, 31, 166, 40];
    /// `6BwHHDg3…`. Position 4 for `buy_exact_in`, 11/11. It changes for sell.
    pub const LAN_PLATFORM: Pubkey = [77, 23, 158, 58, 238, 226, 29, 241, 63, 161, 14, 65, 7, 44, 216, 155, 93, 163, 211, 229, 138, 85, 221, 46, 210, 185, 209, 248, 6, 214, 250, 177];
    /// `2DPAtwB8…`. Position 14.
    pub const LAN_EVENT: Pubkey = [18, 7, 185, 155, 53, 221, 226, 142, 75, 158, 222, 233, 126, 73, 215, 99, 151, 181, 164, 160, 201, 63, 175, 210, 116, 199, 134, 160, 6, 142, 221, 111];
    /// `goonuddtQR…`.
    pub const GOON_PROGRAM: Pubkey = [10, 50, 147, 124, 70, 61, 176, 68, 46, 101, 163, 176, 94, 85, 179, 2, 202, 93, 199, 67, 242, 200, 223, 86, 115, 253, 67, 239, 116, 76, 240, 175];
    /// USDC. Position 8 for direction 1, 19/19.
    pub const GOON_USDC: Pubkey = [198, 250, 122, 243, 190, 219, 173, 58, 61, 101, 243, 106, 171, 201, 116, 49, 177, 187, 228, 194, 210, 246, 224, 228, 124, 166, 2, 3, 69, 47, 93, 97];
    /// `BNrK9LpE…`. Position 10.
    pub const GOON_CFG: Pubkey = [154, 45, 211, 14, 170, 83, 85, 199, 193, 91, 117, 12, 85, 29, 197, 162, 7, 102, 13, 194, 214, 104, 122, 130, 86, 108, 129, 15, 10, 180, 11, 156];
    /// `BSwp6bEB…`.
    pub const BSWP_PROGRAM: Pubkey = [155, 58, 93, 153, 133, 205, 99, 220, 200, 98, 145, 180, 142, 67, 70, 214, 157, 171, 54, 99, 242, 49, 240, 199, 95, 111, 196, 132, 118, 236, 111, 125];
    /// `2QWN6Wjr…`. Position 1, 3/3.
    pub const BSWP_CFG: Pubkey = [20, 225, 63, 71, 136, 20, 161, 133, 239, 207, 18, 134, 220, 8, 142, 128, 61, 16, 208, 224, 15, 0, 95, 57, 113, 80, 162, 247, 12, 26, 152, 208];
    /// `8NyaPDJe…`. Position 13, 3/3.
    pub const BSWP_EVENT: Pubkey = [109, 162, 249, 197, 200, 149, 125, 197, 208, 250, 87, 39, 84, 81, 11, 202, 184, 237, 216, 252, 217, 165, 221, 224, 78, 44, 26, 226, 41, 137, 61, 165];
    /// Sysvar Rent.
    pub const BSWP_RENT: Pubkey = [6, 167, 213, 23, 25, 44, 92, 81, 33, 140, 201, 76, 61, 74, 241, 127, 88, 218, 238, 8, 155, 161, 253, 68, 227, 219, 217, 138, 0, 0, 0, 0];
    /// `ALPHAQmeA…`.
    pub const ALPHA_PROGRAM: Pubkey = [138, 176, 104, 132, 97, 57, 126, 103, 156, 196, 123, 16, 0, 158, 80, 252, 24, 74, 89, 222, 97, 142, 204, 112, 187, 173, 151, 49, 236, 244, 174, 189];
    /// `9W959DqE…`.
    pub const ORCA_PROGRAM: Pubkey = [126, 84, 119, 26, 87, 166, 241, 76, 169, 228, 2, 213, 74, 238, 69, 247, 55, 138, 202, 54, 92, 123, 22, 154, 126, 200, 63, 81, 130, 178, 152, 240];
    /// `SSwapUtyt…`.
    pub const SAROS_PROGRAM: Pubkey = [6, 132, 218, 22, 166, 137, 194, 195, 137, 106, 132, 17, 113, 91, 233, 67, 46, 236, 28, 167, 26, 28, 226, 8, 114, 134, 198, 118, 119, 207, 101, 189];
    /// `SwaPpA9LAaLf…`. One failed leg.
    pub const A9_PROGRAM: Pubkey = [6, 165, 58, 174, 54, 191, 72, 111, 181, 217, 56, 38, 78, 230, 69, 215, 75, 96, 22, 224, 244, 122, 235, 179, 236, 22, 67, 139, 247, 191, 251, 225];
    /// `HYEXCHtH…`.
    pub const HYLO_PROGRAM: Pubkey = [245, 187, 72, 160, 4, 116, 48, 134, 197, 164, 152, 189, 233, 219, 27, 124, 201, 65, 103, 243, 58, 82, 140, 90, 13, 150, 83, 40, 223, 158, 124, 33];
    /// `4VzpNE51…`. Last account of both legs.
    pub const HYLO_TAIL: Pubkey = [52, 0, 178, 106, 46, 18, 83, 56, 123, 59, 141, 26, 19, 233, 52, 19, 58, 10, 248, 161, 246, 172, 45, 191, 0, 118, 249, 200, 179, 145, 40, 124];
    /// Hylo direction 0, position0.
    pub const HY0_0: Pubkey = [245, 187, 72, 160, 4, 116, 48, 134, 197, 164, 152, 189, 233, 219, 27, 124, 201, 65, 103, 243, 58, 82, 140, 90, 13, 150, 83, 40, 223, 158, 124, 33];
    /// Hylo direction 0, position2.
    pub const HY0_2: Pubkey = [2, 98, 74, 197, 110, 200, 170, 127, 86, 115, 232, 163, 180, 43, 12, 195, 167, 34, 210, 205, 34, 179, 15, 18, 50, 124, 97, 178, 114, 72, 77, 246];
    /// Hylo direction 0, position3.
    pub const HY0_3: Pubkey = [194, 73, 184, 157, 239, 232, 41, 241, 95, 226, 99, 61, 248, 239, 226, 12, 103, 157, 44, 118, 73, 150, 108, 205, 116, 141, 223, 61, 128, 155, 126, 177];
    /// Hylo direction 0, position4.
    pub const HY0_4: Pubkey = [120, 225, 127, 249, 207, 158, 242, 139, 21, 227, 213, 131, 39, 65, 236, 231, 128, 78, 7, 213, 201, 127, 66, 239, 51, 237, 95, 5, 156, 116, 146, 95];
    /// Hylo direction 0, position5.
    pub const HY0_5: Pubkey = [99, 73, 51, 88, 178, 1, 253, 237, 215, 31, 103, 210, 236, 154, 50, 164, 92, 154, 60, 252, 51, 209, 114, 61, 172, 205, 142, 111, 126, 61, 238, 137];
    /// Hylo direction 0, position6.
    pub const HY0_6: Pubkey = [44, 230, 184, 120, 29, 170, 175, 236, 112, 168, 219, 237, 64, 153, 144, 87, 85, 82, 63, 59, 63, 87, 238, 4, 230, 89, 59, 225, 124, 111, 144, 52];
    /// Hylo direction 0, position7.
    pub const HY0_7: Pubkey = [127, 253, 144, 191, 10, 140, 56, 59, 33, 45, 224, 177, 163, 178, 213, 86, 102, 217, 213, 158, 171, 201, 188, 228, 138, 219, 171, 109, 167, 114, 129, 102];
    /// Hylo direction 0, position8.
    pub const HY0_8: Pubkey = [153, 90, 104, 213, 208, 104, 149, 198, 189, 37, 253, 149, 233, 58, 71, 122, 101, 169, 94, 176, 90, 131, 79, 25, 72, 9, 111, 249, 18, 214, 70, 75];
    /// Hylo direction 0, position9.
    pub const HY0_9: Pubkey = [236, 243, 197, 107, 141, 148, 225, 122, 138, 46, 75, 149, 120, 230, 107, 102, 118, 3, 187, 127, 128, 175, 75, 3, 253, 7, 216, 20, 25, 186, 137, 68];
    /// Hylo direction 0, position10.
    pub const HY0_10: Pubkey = [111, 233, 97, 71, 155, 218, 49, 142, 190, 170, 89, 222, 121, 157, 103, 142, 96, 135, 133, 78, 170, 77, 104, 139, 59, 67, 246, 201, 232, 6, 217, 27];
    /// Hylo direction 0, position11.
    pub const HY0_11: Pubkey = [215, 138, 185, 165, 247, 110, 119, 66, 203, 151, 120, 235, 24, 24, 82, 161, 232, 75, 183, 26, 218, 69, 115, 196, 148, 41, 164, 8, 17, 175, 194, 35];
    /// Hylo direction 0, position12.
    pub const HY0_12: Pubkey = [182, 137, 190, 124, 145, 245, 126, 104, 88, 6, 24, 112, 141, 201, 171, 232, 18, 37, 88, 17, 128, 214, 191, 115, 193, 114, 61, 221, 118, 197, 115, 178];
    /// Hylo direction 0, position13.
    pub const HY0_13: Pubkey = [128, 33, 124, 203, 147, 204, 191, 73, 63, 10, 135, 221, 219, 234, 103, 124, 55, 8, 218, 198, 115, 202, 63, 109, 12, 190, 188, 197, 188, 64, 36, 206];
    /// Hylo direction 0, position15.
    pub const HY0_15: Pubkey = [52, 0, 178, 106, 46, 18, 83, 56, 123, 59, 141, 26, 19, 233, 52, 19, 58, 10, 248, 161, 246, 172, 45, 191, 0, 118, 249, 200, 179, 145, 40, 124];
    /// Hylo direction 1, position0.
    pub const HY1_0: Pubkey = [245, 187, 72, 160, 4, 116, 48, 134, 197, 164, 152, 189, 233, 219, 27, 124, 201, 65, 103, 243, 58, 82, 140, 90, 13, 150, 83, 40, 223, 158, 124, 33];
    /// Hylo direction 1, position2.
    pub const HY1_2: Pubkey = [18, 116, 135, 116, 97, 137, 0, 248, 178, 122, 73, 120, 80, 187, 250, 219, 120, 19, 41, 61, 58, 249, 109, 251, 144, 118, 14, 88, 73, 59, 224, 238];
    /// Hylo direction 1, position3.
    pub const HY1_3: Pubkey = [194, 73, 184, 157, 239, 232, 41, 241, 95, 226, 99, 61, 248, 239, 226, 12, 103, 157, 44, 118, 73, 150, 108, 205, 116, 141, 223, 61, 128, 155, 126, 177];
    /// Hylo direction 1, position4.
    pub const HY1_4: Pubkey = [67, 119, 55, 28, 116, 248, 110, 161, 139, 241, 127, 177, 96, 51, 132, 82, 170, 99, 250, 220, 172, 213, 163, 31, 179, 172, 245, 75, 165, 46, 136, 173];
    /// Hylo direction 1, position5.
    pub const HY1_5: Pubkey = [99, 73, 51, 88, 178, 1, 253, 237, 215, 31, 103, 210, 236, 154, 50, 164, 92, 154, 60, 252, 51, 209, 114, 61, 172, 205, 142, 111, 126, 61, 238, 137];
    /// Hylo direction 1, position6.
    pub const HY1_6: Pubkey = [44, 230, 184, 120, 29, 170, 175, 236, 112, 168, 219, 237, 64, 153, 144, 87, 85, 82, 63, 59, 63, 87, 238, 4, 230, 89, 59, 225, 124, 111, 144, 52];
    /// Hylo direction 1, position7.
    pub const HY1_7: Pubkey = [127, 253, 144, 191, 10, 140, 56, 59, 33, 45, 224, 177, 163, 178, 213, 86, 102, 217, 213, 158, 171, 201, 188, 228, 138, 219, 171, 109, 167, 114, 129, 102];
    /// Hylo direction 1, position8.
    pub const HY1_8: Pubkey = [153, 90, 104, 213, 208, 104, 149, 198, 189, 37, 253, 149, 233, 58, 71, 122, 101, 169, 94, 176, 90, 131, 79, 25, 72, 9, 111, 249, 18, 214, 70, 75];
    /// Hylo direction 1, position9.
    pub const HY1_9: Pubkey = [173, 103, 235, 169, 12, 25, 214, 27, 75, 203, 33, 146, 88, 254, 2, 155, 19, 80, 42, 83, 86, 209, 49, 223, 152, 181, 78, 187, 117, 78, 76, 53];
    /// Hylo direction 1, position10.
    pub const HY1_10: Pubkey = [236, 243, 197, 107, 141, 148, 225, 122, 138, 46, 75, 149, 120, 230, 107, 102, 118, 3, 187, 127, 128, 175, 75, 3, 253, 7, 216, 20, 25, 186, 137, 68];
    /// Hylo direction 1, position11.
    pub const HY1_11: Pubkey = [33, 238, 44, 187, 134, 132, 34, 27, 4, 72, 210, 209, 57, 90, 66, 0, 87, 118, 64, 68, 189, 233, 241, 227, 79, 134, 122, 198, 120, 232, 216, 161];
    /// Hylo direction 1, position12.
    pub const HY1_12: Pubkey = [215, 138, 185, 165, 247, 110, 119, 66, 203, 151, 120, 235, 24, 24, 82, 161, 232, 75, 183, 26, 218, 69, 115, 196, 148, 41, 164, 8, 17, 175, 194, 35];
    /// Hylo direction 1, position13.
    pub const HY1_13: Pubkey = [248, 2, 140, 90, 189, 112, 70, 187, 81, 186, 115, 57, 226, 42, 249, 213, 1, 61, 193, 164, 199, 133, 119, 182, 188, 50, 60, 58, 177, 164, 252, 129];
    /// Hylo direction 1, position14.
    pub const HY1_14: Pubkey = [120, 225, 127, 249, 207, 158, 242, 139, 21, 227, 213, 131, 39, 65, 236, 231, 128, 78, 7, 213, 201, 127, 66, 239, 51, 237, 95, 5, 156, 116, 146, 95];
    /// Hylo direction 1, position15.
    pub const HY1_15: Pubkey = [128, 33, 124, 203, 147, 204, 191, 73, 63, 10, 135, 221, 219, 234, 103, 124, 55, 8, 218, 198, 115, 202, 63, 109, 12, 190, 188, 197, 188, 64, 36, 206];
    /// Hylo direction 1, position17.
    pub const HY1_17: Pubkey = [52, 0, 178, 106, 46, 18, 83, 56, 123, 59, 141, 26, 19, 233, 52, 19, 58, 10, 248, 161, 246, 172, 45, 191, 0, 118, 249, 200, 179, 145, 40, 124];
}

/// Pump.fun bonding curve (venue 0x0e). Route direction 0 is `buy_exact_quote_in_v2`
/// (28 accounts, 22/22), direction 1 is `sell_v2` (27 accounts, 11/11). No curve, the planner does not pick.
pub mod pump_fun {
    use super::measured_keys as k;
    use super::{metas, pump_amm::ATA_PROGRAM, AccountMeta, Pubkey, SYSTEM};

    pub use k::{
        PUMP_EVENT as EVENT_AUTHORITY, PUMP_FEE_PROGRAM as FEE_PROGRAM, PUMP_FUN_PROGRAM as PROGRAM,
        PUMP_GLOBAL as GLOBAL,
    };

    const BUY_FLAGS: [(bool, bool); 28] = [
        (false, false), (false, false), (false, false), (false, false), (false, false), (false, false), (false, false),
        (false, true), (false, true), (false, true), (false, true), (false, true), (false, true), (false, true),
        (true, true),
        (false, true), (false, true), (false, true), (false, true),
        (false, false), (false, false),
        (false, true), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false),
    ];
    const SELL_FLAGS: [(bool, bool); 27] = [
        (false, false), (false, false), (false, false), (false, false), (false, false), (false, false), (false, false),
        (false, true), (false, true), (false, true), (false, true), (false, true), (false, true), (false, true),
        (true, true),
        (false, true), (false, true), (false, true), (false, true),
        (false, false),
        (false, true), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false),
    ];

    /// `buy_exact_quote_in_v2`. `market` is positions 2, 3, 4, 5, 8, 10, 11, 12, 13, 15, 16, 17, 18, 19, 22.
    pub struct PumpFunBuy {
        pub user: Pubkey,
        pub market: [Pubkey; 15],
    }

    impl PumpFunBuy {
        pub const N_ACCOUNTS: u8 = 28;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 28];
            keys[1] = GLOBAL;
            keys[6] = ATA_PROGRAM;
            keys[7] = k::PUMP_SLOT7;
            keys[9] = k::PUMP_SLOT9;
            keys[14] = self.user;
            keys[20] = k::PUMP_HQ;
            keys[21] = k::PUMP_U4;
            keys[23] = k::PUMP_WF;
            keys[24] = FEE_PROGRAM;
            keys[25] = SYSTEM;
            keys[26] = EVENT_AUTHORITY;
            keys[27] = PROGRAM;
            const IDX: [usize; 15] = [2, 3, 4, 5, 8, 10, 11, 12, 13, 15, 16, 17, 18, 19, 22];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &BUY_FLAGS)
        }
    }

    /// `sell_v2`. `market` is positions 2, 11, 12, 13, 15, 16, 17, 18, 19.
    pub struct PumpFunSell {
        pub user: Pubkey,
        pub market: [Pubkey; 9],
    }

    impl PumpFunSell {
        pub const N_ACCOUNTS: u8 = 27;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 27];
            keys[1] = GLOBAL;
            keys[3] = k::PUMP_WSOL;
            keys[4] = super::TOKEN_2022;
            keys[5] = super::TOKEN;
            keys[6] = ATA_PROGRAM;
            keys[7] = k::PUMP_SLOT7;
            keys[8] = k::PUMP_SLOT94;
            keys[9] = k::PUMP_SLOT9;
            keys[10] = k::PUMP_SLOT_HJ;
            keys[14] = self.user;
            keys[20] = k::PUMP_U4;
            keys[21] = k::PUMP_WX;
            keys[22] = k::PUMP_WF;
            keys[23] = FEE_PROGRAM;
            keys[24] = SYSTEM;
            keys[25] = EVENT_AUTHORITY;
            keys[26] = PROGRAM;
            const IDX: [usize; 9] = [2, 11, 12, 13, 15, 16, 17, 18, 19];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &SELL_FLAGS)
        }
    }
}

/// Raydium LaunchLab (venue 0x13). Direction 0 = `buy_exact_in`, direction 1 = `sell_exact_in`.
/// 19 accounts. No curve, the planner does not pick.
pub mod launchpad {
    use super::measured_keys as k;
    use super::{metas, AccountMeta, Pubkey, SYSTEM, TOKEN_2022};

    pub use k::{LAN_AUTH as AUTHORITY, LAN_EVENT as EVENT_AUTHORITY, LAN_PROGRAM as PROGRAM};

    const FLAGS: [(bool, bool); 19] = [
        (false, false), (true, true), (false, false), (false, false), (false, false),
        (false, true), (false, true), (false, true), (false, true), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false), (false, false), (false, false),
        (false, true), (false, true),
    ];

    /// `buy_exact_in`. `market` is positions 3, 5, 6, 7, 8, 9, 10, 11, 17, 18.
    pub struct LaunchBuy {
        pub user: Pubkey,
        pub market: [Pubkey; 10],
    }

    impl LaunchBuy {
        pub const N_ACCOUNTS: u8 = 19;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 19];
            keys[1] = self.user;
            keys[2] = AUTHORITY;
            keys[4] = k::LAN_PLATFORM;
            keys[12] = TOKEN_2022;
            keys[13] = TOKEN_2022;
            keys[14] = EVENT_AUTHORITY;
            keys[15] = PROGRAM;
            keys[16] = SYSTEM;
            const IDX: [usize; 10] = [3, 5, 6, 7, 8, 9, 10, 11, 17, 18];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }

    /// `sell_exact_in`. `market` is positions 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18.
    pub struct LaunchSell {
        pub user: Pubkey,
        pub market: [Pubkey; 13],
    }

    impl LaunchSell {
        pub const N_ACCOUNTS: u8 = 19;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 19];
            keys[1] = self.user;
            keys[2] = AUTHORITY;
            keys[14] = EVENT_AUTHORITY;
            keys[15] = PROGRAM;
            keys[16] = SYSTEM;
            const IDX: [usize; 13] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }
}

/// GoonFi v2 (venue 0x1b). 14 accounts. Direction 1 has USDC at position 8 (19/19).
/// For direction 0 this position changes. No curve, the planner does not pick.
pub mod goonfi {
    use super::measured_keys as k;
    use super::{dflow::SYSVAR_INSTRUCTIONS, metas, AccountMeta, Pubkey, TOKEN};

    pub use k::{GOON_CFG as CONFIG, GOON_PROGRAM as PROGRAM, GOON_USDC as USDC};

    const FLAGS: [(bool, bool); 14] = [
        (false, false), (true, true),
        (false, true), (false, true), (false, true), (false, true), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false), (false, false), (false, false),
    ];

    /// Route direction 0. The CPI byte is then 1. `market`: positions 2, 3, 4, 5, 6, 7, 8, 9, 12.
    pub struct GoonFiDir0 {
        pub user: Pubkey,
        pub market: [Pubkey; 9],
    }

    impl GoonFiDir0 {
        pub const N_ACCOUNTS: u8 = 14;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 14];
            keys[1] = self.user;
            keys[10] = CONFIG;
            keys[11] = SYSVAR_INSTRUCTIONS;
            keys[13] = TOKEN;
            const IDX: [usize; 9] = [2, 3, 4, 5, 6, 7, 8, 9, 12];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }

    /// Route direction 1. The CPI byte is then 0. `market`: positions 2, 3, 4, 5, 6, 7, 9, 12.
    pub struct GoonFiDir1 {
        pub user: Pubkey,
        pub market: [Pubkey; 8],
    }

    impl GoonFiDir1 {
        pub const N_ACCOUNTS: u8 = 14;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 14];
            keys[1] = self.user;
            keys[8] = USDC;
            keys[10] = CONFIG;
            keys[11] = SYSVAR_INSTRUCTIONS;
            keys[13] = TOKEN;
            const IDX: [usize; 8] = [2, 3, 4, 5, 6, 7, 9, 12];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }
}

/// BSwp (venue 0x14). Direction 0 only, 18 accounts, 3/3 legs. No curve, the planner does not pick.
pub mod bswp {
    use super::measured_keys as k;
    use super::{metas, pump_amm::ATA_PROGRAM, AccountMeta, Pubkey, SYSTEM, TOKEN};

    pub use k::{BSWP_CFG as CONFIG, BSWP_EVENT as EVENT_AUTHORITY, BSWP_PROGRAM as PROGRAM, BSWP_RENT as RENT};

    const FLAGS: [(bool, bool); 18] = [
        (false, false), (false, false),
        (false, true),
        (false, false), (false, false),
        (false, true), (false, true), (false, true), (false, true),
        (true, true),
        (false, true), (false, true),
        (true, true),
        (false, false), (false, false), (false, false), (false, false), (false, false),
    ];

    /// `market` is positions 2, 3, 4, 5, 6, 7, 8, 10, 11. The user is at 9 and 12.
    pub struct BswpHop {
        pub user: Pubkey,
        pub market: [Pubkey; 9],
    }

    impl BswpHop {
        pub const N_ACCOUNTS: u8 = 18;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 18];
            keys[1] = CONFIG;
            keys[9] = self.user;
            keys[12] = self.user;
            keys[13] = EVENT_AUTHORITY;
            keys[14] = SYSTEM;
            keys[15] = TOKEN;
            keys[16] = ATA_PROGRAM;
            keys[17] = RENT;
            const IDX: [usize; 9] = [2, 3, 4, 5, 6, 7, 8, 10, 11];
            for (i, pk) in IDX.into_iter().zip(self.market) {
                keys[i] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }
}

/// Orca token-swap v2, Saros AMM and SwaPpA9L (venue 0x15, 0x0d, 0x1d).
/// Same layout of 11 accounts. The program is supplied by the caller. No curve, the planner does not pick.
pub mod token_swap {
    use super::measured_keys as k;
    use super::{metas, AccountMeta, Pubkey, TOKEN};

    pub use k::{A9_PROGRAM as SWAP_A9, ORCA_PROGRAM as ORCA_V2, SAROS_PROGRAM as SAROS};

    const FLAGS: [(bool, bool); 11] = [
        (false, false),
        (false, true), (false, true),
        (true, true),
        (false, true), (false, true), (false, true), (false, true), (false, true), (false, true),
        (false, false),
    ];

    pub struct TokenSwapHop {
        pub program: Pubkey,
        pub pool: Pubkey,
        pub authority: Pubkey,
        pub user: Pubkey,
        pub user_source: Pubkey,
        pub pool_source: Pubkey,
        pub pool_destination: Pubkey,
        pub user_destination: Pubkey,
        pub pool_mint: Pubkey,
        pub fee_account: Pubkey,
    }

    impl TokenSwapHop {
        pub const N_ACCOUNTS: u8 = 11;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            metas(
                &[
                    self.program,
                    self.pool,
                    self.authority,
                    self.user,
                    self.user_source,
                    self.pool_source,
                    self.pool_destination,
                    self.user_destination,
                    self.pool_mint,
                    self.fee_account,
                    TOKEN,
                ],
                &FLAGS,
            )
        }
    }
}

/// AlphaQ (venue 0x21). Direction 0 only, 13 accounts. The three legs of the window are one market.
/// Positions 2..=10 are supplied by the caller. No curve, the planner does not pick.
pub mod alphaq {
    use super::measured_keys as k;
    use super::{dflow::SYSVAR_INSTRUCTIONS, metas, AccountMeta, Pubkey, TOKEN};

    pub use k::ALPHA_PROGRAM as PROGRAM;

    const FLAGS: [(bool, bool); 13] = [
        (false, false), (true, true), (false, false),
        (false, true), (false, true), (false, true), (false, true),
        (false, true), (false, true), (false, true), (false, true),
        (false, false), (false, false),
    ];

    pub struct AlphaQHop {
        pub user: Pubkey,
        /// Positions 2..=10.
        pub market: [Pubkey; 9],
    }

    impl AlphaQHop {
        pub const N_ACCOUNTS: u8 = 13;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            let mut keys = [PROGRAM; 13];
            keys[1] = self.user;
            keys[11] = TOKEN;
            keys[12] = SYSVAR_INSTRUCTIONS;
            for (i, pk) in self.market.into_iter().enumerate() {
                keys[i + 2] = pk;
            }
            metas(&keys, &FLAGS)
        }
    }
}

/// Hylo (venue 0x29). Two constant lists from the binary (the hyUSD / xSOL mints are in rodata).
/// The user is an argument. No curve, the planner does not pick.
pub mod hylo {
    use super::measured_keys as k;
    use super::{metas, AccountMeta, Pubkey, TOKEN};

    pub use k::{HYLO_PROGRAM as PROGRAM, HYLO_TAIL as TAIL};

    const FLAGS0: [(bool, bool); 16] = [
        (false, false), (true, true),
        (false, true), (false, true), (false, false), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false),
        (false, true), (false, true),
        (false, false), (false, false), (false, false),
    ];
    const FLAGS1: [(bool, bool); 18] = [
        (false, false), (true, true),
        (false, true), (false, true), (false, true), (false, true), (false, true),
        (false, false), (false, false), (false, false), (false, false), (false, false), (false, false),
        (false, true),
        (false, false), (false, false), (false, false), (false, false),
    ];

    /// Direction 0. 16 accounts, 2/2.
    pub struct HyloLeg0 {
        pub user: Pubkey,
    }

    impl HyloLeg0 {
        pub const N_ACCOUNTS: u8 = 16;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            metas(
                &[
                    k::HY0_0, self.user, k::HY0_2, k::HY0_3, k::HY0_4, k::HY0_5, k::HY0_6, k::HY0_7,
                    k::HY0_8, k::HY0_9, k::HY0_10, k::HY0_11, k::HY0_12, k::HY0_13, TOKEN, k::HY0_15,
                ],
                &FLAGS0,
            )
        }
    }

    /// Direction 1. 18 accounts, 5/5.
    pub struct HyloLeg1 {
        pub user: Pubkey,
    }

    impl HyloLeg1 {
        pub const N_ACCOUNTS: u8 = 18;

        pub fn accounts(&self) -> Vec<AccountMeta> {
            metas(
                &[
                    k::HY1_0, self.user, k::HY1_2, k::HY1_3, k::HY1_4, k::HY1_5, k::HY1_6, k::HY1_7,
                    k::HY1_8, k::HY1_9, k::HY1_10, k::HY1_11, k::HY1_12, k::HY1_13, k::HY1_14, k::HY1_15,
                    TOKEN, k::HY1_17,
                ],
                &FLAGS1,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(n: u8) -> Pubkey {
        let mut k = [0u8; 32];
        k[0] = n;
        k[31] = n;
        k
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn pk_at(bytes: &[u8], i: usize) -> Pubkey {
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes[i * 32..i * 32 + 32]);
        k
    }

    #[test]
    fn invariant_holds_two_hop() {
        let prefix = fixed_prefix(pk(1), pk(2), pk(3), pk(4), pk(5), pk(6));
        let inter = [intermediate_triple(pk(7), pk(8), pk(9))];
        let hop0 = vec![m(pk(10), false, false); 6];
        let hop1 = vec![m(pk(11), false, false); 9];
        let out = assemble_route(&prefix, &inter, &[hop0, hop1], &[6, 9]).unwrap();
        // 7 + 3 + 6 + 9 = 25 = (6+9) + 3*2 + 4
        assert_eq!(out.len(), 25);
        assert_eq!(out.len(), 6 + 9 + 3 * 2 + 4);
    }

    #[test]
    fn rejects_bad_hop_len_and_counts() {
        let prefix = fixed_prefix(pk(1), pk(2), pk(3), pk(4), pk(5), pk(6));
        let inter = [intermediate_triple(pk(7), pk(8), pk(9))];
        let hop0 = vec![m(pk(10), false, false); 6];
        let hop1 = vec![m(pk(11), false, false); 9];
        // wrong declared length of leg 0
        assert_eq!(
            assemble_route(&prefix, &inter, &[hop0.clone(), hop1.clone()], &[5, 9]),
            Err(RouteError::HopLenMismatch { hop: 0, declared: 5, actual: 6 })
        );
        // wrong number of intermediate triples (0 instead of 1)
        assert_eq!(
            assemble_route(&prefix, &[], &[hop0, hop1], &[6, 9]),
            Err(RouteError::IntermediateCount)
        );
    }

    #[test]
    fn pump_hop_matches_real_tx() {
        // 28 accounts of a real Pump AMM leg (tx 5vyoG18jr…), dumped from the set of real tx.
        let raw = hex(include_str!("testdata/pump_hop_accounts.hex").trim());
        assert_eq!(raw.len(), 28 * 32);
        let want: Vec<Pubkey> = (0..28).map(|i| pk_at(&raw, i)).collect();

        // We assemble the leg from fields supplied on the fly by the pool decoder + prefix + resolver.
        let hop = pump_amm::PumpAmmHop {
            pool: want[1],
            base_mint: want[4],
            quote_mint: want[5],
            base_vault: want[8],
            quote_vault: want[9],
            user: want[2],
            user_base_ata: want[6],
            user_quote_ata: want[7],
            fee_recipient: want[10],
            fee_recipient_ata: want[11],
            base_token_program: want[12],
            quote_token_program: want[13],
            creator_vault_ata: want[18],
            creator_vault_authority: want[19],
            user_volume_accumulator: want[21],
            extra: [
                m(want[24], false, false),
                m(want[25], false, false),
                m(want[26], false, false),
                m(want[27], false, false),
            ],
        };
        let got = hop.accounts();
        assert_eq!(got.len(), pump_amm::PumpAmmHop::N_ACCOUNTS as usize);
        // The account order must reproduce the real slice key for key.
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.pubkey, *w, "account at position {i} does not match the real tx");
        }
        // The program constants hardcoded in the module must match the real tx at the right positions.
        assert_eq!(got[0].pubkey, pump_amm::PROGRAM);
        assert_eq!(got[3].pubkey, pump_amm::GLOBAL_CONFIG);
        assert_eq!(got[16].pubkey, pump_amm::EVENT_AUTHORITY);
        assert_eq!(got[17].pubkey, pump_amm::PROGRAM);
        assert_eq!(got[20].pubkey, pump_amm::GLOBAL_VOLUME_ACCUMULATOR);
        assert_eq!(got[22].pubkey, pump_amm::FEE_CONFIG);
        assert_eq!(got[23].pubkey, pump_amm::FEE_PROGRAM);
        // First account of the slice = DEX program ID (route interpreter convention).
        assert_eq!(got[0].pubkey, pump_amm::PROGRAM);
        // user is the only signer, pool writable.
        assert!(got[2].is_signer && got[2].is_writable);
        assert!(got[1].is_writable && !got[1].is_signer);
    }

    /// Loads a fixture (concatenated hex of 32-byte accounts) as a list of keys.
    fn slice_pubkeys(hexstr: &str) -> Vec<Pubkey> {
        let raw = hex(hexstr.trim());
        (0..raw.len() / 32).map(|i| pk_at(&raw, i)).collect()
    }

    /// Checks that the assembled slice reproduces the real fixture key for key at every position.
    fn assert_matches(got: &[AccountMeta], want: &[Pubkey], venue: &str) {
        assert_eq!(got.len(), want.len(), "{venue}: account count");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g.pubkey, *w, "{venue}: account at position {i}");
        }
    }

    #[test]
    fn dlmm_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/dlmm_hop_accounts.hex"));
        assert_eq!(want.len(), 18, "modal fixture = 18 accounts (1 BinArray)");
        let hop = dlmm::DlmmHop {
            lb_pair: want[1],
            bitmap_extension: want[2],
            reserve_x: want[3],
            reserve_y: want[4],
            user_token_in: want[5],
            user_token_out: want[6],
            token_x_mint: want[7],
            token_y_mint: want[8],
            oracle: want[9],
            host_fee_in: want[10],
            user: want[11],
            token_x_program: want[12],
            token_y_program: want[13],
            bin_arrays: want[17..].to_vec(),
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "DLMM");
        assert_eq!(got[0].pubkey, dlmm::PROGRAM);
        assert_eq!(got[14].pubkey, MEMO);
        assert_eq!(got[15].pubkey, dlmm::EVENT_AUTHORITY);
        assert!(got[11].is_signer, "user signs");
        assert!(got[17].is_writable, "BinArray writable");
    }

    #[test]
    fn cpmm_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/cpmm_hop_accounts.hex"));
        assert_eq!(want.len(), cpmm::CpmmHop::N_ACCOUNTS as usize);
        let hop = cpmm::CpmmHop {
            payer: want[1],
            amm_config: want[3],
            pool_state: want[4],
            input_token_account: want[5],
            output_token_account: want[6],
            input_vault: want[7],
            output_vault: want[8],
            input_token_program: want[9],
            output_token_program: want[10],
            input_token_mint: want[11],
            output_token_mint: want[12],
            observation_state: want[13],
        };
        assert_matches(&hop.accounts(), &want, "CPMM");
        assert_eq!(hop.accounts()[2].pubkey, cpmm::AUTHORITY);
    }

    #[test]
    fn whirlpool_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/whirlpool_hop_accounts.hex"));
        assert_eq!(want.len(), 18, "fixture = 18 (16 core + 2 additional tick_array)");
        let hop = whirlpool::WhirlpoolHop {
            token_program_a: want[1],
            token_program_b: want[2],
            token_authority: want[4],
            whirlpool: want[5],
            token_mint_a: want[6],
            token_mint_b: want[7],
            token_owner_a: want[8],
            token_vault_a: want[9],
            token_owner_b: want[10],
            token_vault_b: want[11],
            tick_array_0: want[12],
            tick_array_1: want[13],
            tick_array_2: want[14],
            oracle: want[15],
            supplemental_tick_arrays: want[16..].to_vec(),
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "Whirlpool");
        assert_eq!(got[0].pubkey, whirlpool::PROGRAM);
        assert_eq!(got[3].pubkey, MEMO);
        assert!(got[4].is_signer, "token_authority signs");
    }

    #[test]
    fn damm_v2_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/damm_v2_hop_accounts.hex"));
        assert_eq!(want.len(), damm_v2::DammV2Hop::N_ACCOUNTS as usize);
        let hop = damm_v2::DammV2Hop {
            pool: want[2],
            input_token_account: want[3],
            output_token_account: want[4],
            token_a_vault: want[5],
            token_b_vault: want[6],
            token_a_mint: want[7],
            token_b_mint: want[8],
            payer: want[9],
            token_a_program: want[10],
            token_b_program: want[11],
            referral_token_account: want[12],
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "DAMM v2");
        assert_eq!(got[1].pubkey, damm_v2::POOL_AUTHORITY);
        assert_eq!(got[13].pubkey, damm_v2::EVENT_AUTHORITY);
    }

    #[test]
    fn manifest_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/manifest_hop_accounts.hex"));
        assert_eq!(want.len(), manifest::ManifestHop::N_ACCOUNTS as usize);
        let hop = manifest::ManifestHop {
            payer: want[1],
            owner: want[2],
            market: want[3],
            trader_base: want[5],
            trader_quote: want[6],
            base_vault: want[7],
            quote_vault: want[8],
            token_program_base: want[9],
            base_mint: want[10],
            token_program_quote: want[11],
            quote_mint: want[12],
            global: want[13],
            global_vault: want[14],
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "Manifest");
        assert_eq!(got[0].pubkey, manifest::PROGRAM);
        assert_eq!(got[4].pubkey, SYSTEM);
    }

    #[test]
    fn manifest_same_program_shortens_the_list() {
        let mut hop = manifest::ManifestHop {
            payer: pk(1),
            owner: pk(1),
            market: pk(2),
            trader_base: pk(3),
            trader_quote: pk(4),
            base_vault: pk(5),
            quote_vault: pk(6),
            token_program_base: TOKEN,
            base_mint: pk(7),
            token_program_quote: TOKEN,
            quote_mint: pk(8),
            global: pk(9),
            global_vault: pk(10),
        };
        let spl = hop.accounts();
        assert_eq!(spl.len(), 13);
        assert_eq!(spl[9].pubkey, TOKEN);
        assert_eq!(spl[10].pubkey, pk(8), "the quote mint stays, not base");
        assert_eq!(spl[11].pubkey, pk(9));
        assert_eq!(spl[12].pubkey, pk(10));
        assert!(spl.iter().all(|a| a.pubkey != pk(7)));

        hop.token_program_base = TOKEN_2022;
        hop.token_program_quote = TOKEN_2022;
        let t22 = hop.accounts();
        assert_eq!(t22.len(), 14);
        assert_eq!(t22[9].pubkey, TOKEN_2022);
        assert_eq!(t22[10].pubkey, pk(7), "both Token-2022: base mint stays");
        assert_eq!(t22[11].pubkey, pk(8));
        assert_eq!(t22[12].pubkey, pk(9));
        assert_eq!(t22[13].pubkey, pk(10));
    }

    #[test]
    fn clmm_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/clmm_hop_accounts.hex"));
        assert_eq!(want.len(), 17, "modal fixture = 17 (14 core + 3 tick_array)");
        let hop = clmm::ClmmHop {
            program: clmm::PROGRAM,
            payer: want[1],
            amm_config: want[2],
            pool_state: want[3],
            input_token_account: want[4],
            output_token_account: want[5],
            input_vault: want[6],
            output_vault: want[7],
            observation_state: want[8],
            input_vault_mint: want[12],
            output_vault_mint: want[13],
            tick_arrays: want[14..].to_vec(),
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "CLMM");
        assert_eq!(got[9].pubkey, TOKEN);
        assert_eq!(got[10].pubkey, TOKEN_2022);
        assert_eq!(got[11].pubkey, MEMO);
        assert!(got[14].is_writable, "tick_array writable");
    }

    #[test]
    fn dlmm_v1_drops_only_the_memo() {
        let want = slice_pubkeys(include_str!("testdata/dlmm_hop_accounts.hex"));
        let hop = dlmm::DlmmHop {
            lb_pair: want[1],
            bitmap_extension: want[2],
            reserve_x: want[3],
            reserve_y: want[4],
            user_token_in: want[5],
            user_token_out: want[6],
            token_x_mint: want[7],
            token_y_mint: want[8],
            oracle: want[9],
            host_fee_in: want[10],
            user: want[11],
            token_x_program: want[12],
            token_y_program: want[13],
            bin_arrays: want[17..].to_vec(),
        };
        let v2 = hop.accounts();
        let v1 = hop.accounts_v1();
        assert_eq!(v1.len(), 17);
        assert_eq!(&v1[..14], &v2[..14]);
        assert_eq!(&v1[14..], &v2[15..]);
        assert_eq!(v1[14].pubkey, dlmm::EVENT_AUTHORITY);
        assert_eq!(v1[15].pubkey, dlmm::PROGRAM);
    }

    #[test]
    fn clmm_v1_drops_token2022_memo_and_mints() {
        let want = slice_pubkeys(include_str!("testdata/clmm_hop_accounts.hex"));
        let hop = clmm::ClmmHop {
            program: clmm::PROGRAM,
            payer: want[1],
            amm_config: want[2],
            pool_state: want[3],
            input_token_account: want[4],
            output_token_account: want[5],
            input_vault: want[6],
            output_vault: want[7],
            observation_state: want[8],
            input_vault_mint: want[12],
            output_vault_mint: want[13],
            tick_arrays: want[14..].to_vec(),
        };
        let v2 = hop.accounts();
        let v1 = hop.accounts_v1();
        assert_eq!(v1.len(), 13);
        assert_eq!(&v1[..10], &v2[..10]);
        assert_eq!(&v1[10..], &v2[14..]);
        assert_eq!(v1[9].pubkey, TOKEN);
        assert!(v1[10].is_writable, "the tick tail stays writable");
    }

    #[test]
    fn whirlpool_v1_is_the_legacy_swap() {
        let want = slice_pubkeys(include_str!("testdata/whirlpool_hop_accounts.hex"));
        let hop = whirlpool::WhirlpoolHop {
            token_program_a: want[1],
            token_program_b: want[2],
            token_authority: want[4],
            whirlpool: want[5],
            token_mint_a: want[6],
            token_mint_b: want[7],
            token_owner_a: want[8],
            token_vault_a: want[9],
            token_owner_b: want[10],
            token_vault_b: want[11],
            tick_array_0: want[12],
            tick_array_1: want[13],
            tick_array_2: want[14],
            oracle: want[15],
            supplemental_tick_arrays: want[16..].to_vec(),
        };
        let v1 = hop.accounts_v1();
        assert_eq!(v1.len(), 12);
        assert_eq!(v1[0].pubkey, whirlpool::PROGRAM);
        assert_eq!(v1[1].pubkey, want[1]);
        assert_eq!(v1[2].pubkey, want[4]);
        assert!(v1[2].is_signer);
        assert_eq!(v1[3].pubkey, want[5]);
        assert_eq!(v1[4].pubkey, want[8]);
        assert_eq!(v1[5].pubkey, want[9]);
        assert_eq!(v1[6].pubkey, want[10]);
        assert_eq!(v1[7].pubkey, want[11]);
        assert_eq!(v1[8].pubkey, want[12]);
        assert_eq!(v1[9].pubkey, want[13]);
        assert_eq!(v1[10].pubkey, want[14]);
        assert_eq!(v1[11].pubkey, want[15]);
    }

    #[test]
    fn ray_v4_hop_is_swap_base_in_v2() {
        let hop = ray_v4::RayV4Hop {
            amm: pk(1),
            coin_vault: pk(2),
            pc_vault: pk(3),
            user_source: pk(4),
            user_destination: pk(5),
            user_owner: pk(6),
        };
        let got = hop.accounts();
        assert_eq!(got.len(), ray_v4::RayV4Hop::N_ACCOUNTS as usize);
        assert_eq!(got[0].pubkey, ray_v4::PROGRAM);
        assert_eq!(got[1].pubkey, TOKEN);
        assert_eq!(got[2].pubkey, pk(1));
        assert!(got[2].is_writable);
        assert_eq!(got[3].pubkey, ray_v4::AUTHORITY);
        assert!(!got[3].is_writable);
        assert_eq!(got[4].pubkey, pk(2));
        assert_eq!(got[5].pubkey, pk(3));
        assert!(got[4].is_writable && got[6].is_writable && got[7].is_writable);
        assert!(got[8].is_signer);
        assert!(!got[8].is_writable);
    }

    #[test]
    fn futarchy_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/futarchy_hop_accounts.hex"));
        assert_eq!(want.len(), futarchy::FutarchyHop::N_ACCOUNTS as usize);
        let hop = futarchy::FutarchyHop {
            dao: want[1],
            user_base: want[2],
            user_quote: want[3],
            amm_base_vault: want[4],
            amm_quote_vault: want[5],
            user: want[6],
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "Futarchy");
        assert_eq!(got[0].pubkey, futarchy::PROGRAM);
        assert_eq!(got[7].pubkey, TOKEN);
        assert_eq!(got[8].pubkey, futarchy::EVENT_AUTHORITY);
        assert_eq!(got[9].pubkey, futarchy::PROGRAM);
        assert!(got[1].is_writable && got[4].is_writable && got[5].is_writable);
        assert!(got[6].is_signer && !got[6].is_writable);
        assert!(!got[8].is_writable && !got[8].is_signer);
    }

    #[test]
    fn dflow_hop_matches_real_tx() {
        let want = slice_pubkeys(include_str!("testdata/dflow_hop_accounts.hex"));
        assert_eq!(want.len(), dflow::DflowHop::N_ACCOUNTS as usize);
        let hop = dflow::DflowHop {
            user: want[4],
            market: [want[13], want[14], want[15], want[16], want[17]],
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "DFlow");
        assert_eq!(got[0].pubkey, dflow::PROGRAM);
        assert_eq!(got[2].pubkey, dflow::ATA_PROGRAM);
        assert_eq!(got[5].pubkey, dflow::EVENT_AUTHORITY);
        assert_eq!(got[7].pubkey, dflow::SYSVAR_INSTRUCTIONS);
        assert_eq!(got[8].pubkey, TOKEN_2022);
        assert_eq!(got[11].pubkey, dflow::BISON);
        assert_eq!(got[18].pubkey, TOKEN);
        assert!(got[4].is_signer && !got[4].is_writable);
        assert!(got[12].is_signer && got[12].pubkey == want[4]);
        assert!(got[13].is_writable && got[15].is_writable && got[17].is_writable);
        assert!(!got[5].is_writable && !got[7].is_writable && !got[11].is_writable);
    }

    fn picked<const N: usize>(want: &[Pubkey], idx: &[usize]) -> [Pubkey; N] {
        let v: Vec<Pubkey> = idx.iter().map(|&i| want[i]).collect();
        v.try_into().unwrap()
    }

    fn flag_str(got: &[AccountMeta]) -> String {
        got.iter()
            .map(|g| format!("{}{}", if g.is_signer { "S" } else { "-" }, if g.is_writable { "W" } else { "-" }))
            .collect()
    }

    #[test]
    fn pump_fun_hops_match_window() {
        let buy = slice_pubkeys(include_str!("testdata/pump_fun_buy_hop.hex"));
        let hop = pump_fun::PumpFunBuy {
            user: buy[14],
            market: picked(&buy, &[2, 3, 4, 5, 8, 10, 11, 12, 13, 15, 16, 17, 18, 19, 22]),
        };
        let got = hop.accounts();
        assert_matches(&got, &buy, "Pump.fun buy");
        assert_eq!(
            flag_str(&got),
            "---------------W-W-W-W-W-W-WSW-W-W-W-W-----W-W----------"
        );
        assert_eq!(got[0].pubkey, pump_fun::PROGRAM);
        assert_eq!(got[27].pubkey, pump_fun::PROGRAM);

        let sell = slice_pubkeys(include_str!("testdata/pump_fun_sell_hop.hex"));
        let hop = pump_fun::PumpFunSell {
            user: sell[14],
            market: picked(&sell, &[2, 11, 12, 13, 15, 16, 17, 18, 19]),
        };
        let got = hop.accounts();
        assert_matches(&got, &sell, "Pump.fun sell");
        assert_eq!(
            flag_str(&got),
            "---------------W-W-W-W-W-W-WSW-W-W-W-W---W-W----------"
        );
        assert!(got[14].is_signer && got[14].is_writable);
    }

    #[test]
    fn launchpad_hops_match_window() {
        let buy = slice_pubkeys(include_str!("testdata/launchpad_buy_hop.hex"));
        let hop = launchpad::LaunchBuy {
            user: buy[1],
            market: picked(&buy, &[3, 5, 6, 7, 8, 9, 10, 11, 17, 18]),
        };
        let got = hop.accounts();
        assert_matches(&got, &buy, "LaunchLab buy");
        let sell = slice_pubkeys(include_str!("testdata/launchpad_sell_hop.hex"));
        let hop = launchpad::LaunchSell {
            user: sell[1],
            market: picked(&sell, &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18]),
        };
        let got = hop.accounts();
        assert_matches(&got, &sell, "LaunchLab sell");
        assert_eq!(flag_str(&got), "--SW-------W-W-W-W-W---------------W-W");
        assert!(got[1].is_signer);
        assert_eq!(got[16].pubkey, SYSTEM);
    }

    #[test]
    fn goonfi_hops_match_window() {
        let want = slice_pubkeys(include_str!("testdata/goonfi_hop.hex"));
        let dir0 = goonfi::GoonFiDir0 {
            user: want[1],
            market: picked(&want, &[2, 3, 4, 5, 6, 7, 8, 9, 12]),
        };
        let dir1 = goonfi::GoonFiDir1 {
            user: want[1],
            market: picked(&want, &[2, 3, 4, 5, 6, 7, 9, 12]),
        };
        assert_matches(&dir0.accounts(), &want, "GoonFi 0");
        assert_matches(&dir1.accounts(), &want, "GoonFi 1");
        assert_eq!(dir1.accounts()[8].pubkey, goonfi::USDC);
        assert_eq!(flag_str(&dir0.accounts()), "--SW-W-W-W-W-W--------------");
    }

    #[test]
    fn bswp_and_alphaq_match_window() {
        let want = slice_pubkeys(include_str!("testdata/bswp_hop.hex"));
        let hop = bswp::BswpHop {
            user: want[9],
            market: picked(&want, &[2, 3, 4, 5, 6, 7, 8, 10, 11]),
        };
        let got = hop.accounts();
        assert_matches(&got, &want, "BSwp");
        assert_eq!(flag_str(&got), "-----W-----W-W-W-WSW-W-WSW----------");
        assert!(got[9].is_signer && got[12].is_signer);
        assert_eq!(got[9].pubkey, got[12].pubkey);

        let want = slice_pubkeys(include_str!("testdata/alphaq_hop.hex"));
        let hop = alphaq::AlphaQHop { user: want[1], market: picked(&want, &[2, 3, 4, 5, 6, 7, 8, 9, 10]) };
        let got = hop.accounts();
        assert_matches(&got, &want, "AlphaQ");
        assert_eq!(flag_str(&got), "--SW---W-W-W-W-W-W-W-W----");
        assert_eq!(got[11].pubkey, TOKEN);
    }

    #[test]
    fn hylo_legs_match_window() {
        let a = slice_pubkeys(include_str!("testdata/hylo_dir0_hop.hex"));
        let b = slice_pubkeys(include_str!("testdata/hylo_dir1_hop.hex"));
        let g0 = hylo::HyloLeg0 { user: a[1] }.accounts();
        let g1 = hylo::HyloLeg1 { user: b[1] }.accounts();
        assert_matches(&g0, &a, "Hylo 0");
        assert_matches(&g1, &b, "Hylo 1");
        assert_eq!(flag_str(&g0), "--SW-W-W---W-----------W-W------");
        assert_eq!(flag_str(&g1), "--SW-W-W-W-W-W-------------W--------");
        assert_eq!(g0[0].pubkey, hylo::PROGRAM);
        assert_eq!(g1[17].pubkey, hylo::TAIL);
    }

    #[test]
    fn spl_token_swap_forks_match_window_and_a_later_tx() {
        fn hop(want: &[Pubkey], program: Pubkey) -> token_swap::TokenSwapHop {
            token_swap::TokenSwapHop {
                program,
                pool: want[1],
                authority: want[2],
                user: want[3],
                user_source: want[4],
                pool_source: want[5],
                pool_destination: want[6],
                user_destination: want[7],
                pool_mint: want[8],
                fee_account: want[9],
            }
        }
        let window = slice_pubkeys(include_str!("testdata/orca_token_swap_hop.hex"));
        let recent = slice_pubkeys(include_str!("testdata/orca_token_swap_recent_hop.hex"));
        let saros = slice_pubkeys(include_str!("testdata/saros_hop.hex"));
        let a9 = slice_pubkeys(include_str!("testdata/swappa9_hop.hex"));
        for (want, program) in [
            (window.as_slice(), token_swap::ORCA_V2),
            (recent.as_slice(), token_swap::ORCA_V2),
            (saros.as_slice(), token_swap::SAROS),
            (a9.as_slice(), token_swap::SWAP_A9),
        ] {
            let got = hop(want, program).accounts();
            assert_matches(&got, want, "token-swap");
            assert_eq!(flag_str(&got), "---W-WSW-W-W-W-W-W-W--");
            assert_eq!(got[10].pubkey, TOKEN);
            assert!(got[3].is_signer && got[3].is_writable);
        }
    }

}
