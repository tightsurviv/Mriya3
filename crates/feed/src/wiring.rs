//! Example **multi-venue** implementation of the runner seams
//! ([`crate::run::PoolBuilder`] + [`crate::run::RoutePlanner`]): a single registry
//! holds pools from different exchanges, and a single route can **mix venues** (e.g. buy on CPMM, sell
//! on DLMM) in one transaction.
//!
//! Architecture: a shared bot context ([`BotCtx`]) + per-venue pool descriptions ([`CpmmPoolSpec`],
//! [`DlmmPoolSpec`]), wrapped in the [`PoolSpec`] enum with a uniform interface (`decode` / `hop` /
//! `account_keys`). [`Registry`] dispatches: by the pool `id` from the opportunity it picks the right variant and
//! builds its leg through the matching `feed::route::*_hop`. Adding another venue = one enum variant
//! + its `*_hop` (CLMM/Whirlpool add their own variable tail of tick arrays).
//!
//! Still **without signing and without sending** — that is the operator's [`crate::run::Submitter`] seam.

use crate::route;
use crate::run::{PlannedRoute, PoolBuilder, RoutePlanner};
use crate::AccountSource;
use engine::encode::PlanHop;
use engine::{Opportunity, Pool, TriOpportunity};
use keys::venue_pda as vp;
use send::route::{damm_v1, intermediate_triple, ray_v4, TOKEN, TOKEN_2022};
use send::tx::{AccountMeta, Pubkey};

/// Venue code for CPMM in the on-chain program's route interpreter.
pub const VENUE_CPMM: u8 = 0x03;
/// Venue code for DLMM `swap2`.
pub const VENUE_DLMM: u8 = 0x01;
/// Venue code for DLMM `swap` (both programs are SPL Token).
pub const VENUE_DLMM_V1: u8 = 0x23;
/// Venue code for Raydium CLMM `swap_v2`.
pub const VENUE_CLMM: u8 = 0x02;
/// Venue code for Raydium CLMM `swap` (both programs are SPL Token).
pub const VENUE_CLMM_V1: u8 = 0x24;
/// Venue code for Orca Whirlpool `swap_v2`.
pub const VENUE_WHIRLPOOL: u8 = 0x04;
/// Venue code for Orca Whirlpool `swap` (both programs are SPL Token).
pub const VENUE_WHIRLPOOL_V1: u8 = 0x25;
/// Venue code for Meteora DAMM v2.
pub const VENUE_DAMM_V2: u8 = 0x08;
/// Venue code for Pump AMM.
pub const VENUE_PUMP: u8 = 0x05;
/// Venue code for Raydium AMM v4 `SwapBaseInV2`.
pub const VENUE_RAY_V4: u8 = 0x26;
/// Venue code for Meteora DAMM v1.
pub const VENUE_DAMM_V1: u8 = 0x00;
/// Venue code for Pancake CLMM `swap_v2`.
pub const VENUE_PANCAKE: u8 = 0x07;
/// Venue code for Pancake CLMM `swap`.
pub const VENUE_PANCAKE_V1: u8 = 0x27;
/// Venue code for Byreal. Always the long account list.
pub const VENUE_BYREAL: u8 = 0x0a;
/// Venue code for Manifest. Direction 1 = `is_base_in` (input is base).
pub const VENUE_MANIFEST: u8 = 0x10;

/// Bot context shared by all legs: the bot key (payer/authority) + the cycle base + a map
/// `mint → bot token account` (base ATAs and intermediate accounts — the latter are NOT ATAs, they are supplied
/// by the planner/runner).
#[derive(Clone, Debug)]
pub struct BotCtx {
    pub bot: Pubkey,
    pub base_mint: Pubkey,
    pub token_accounts: Vec<(Pubkey, Pubkey)>,
}

impl BotCtx {
    pub fn token_account(&self, mint: &Pubkey) -> Option<Pubkey> {
        self.token_accounts.iter().find(|(m, _)| m == mint).map(|(_, a)| *a)
    }
}

/// Token-2022 in either leg selects the longer v2 instruction. Plain SPL Token selects v1.
fn uses_token_2022(a: Pubkey, b: Pubkey) -> bool {
    a == TOKEN_2022 || b == TOKEN_2022
}

/// Static description of a CPMM pool. Reserves are read live from the balances of both vaults (constant-product).
#[derive(Clone, Debug)]
pub struct CpmmPoolSpec {
    pub pool_state: Pubkey,
    pub amm_config: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub token_program_a: Pubkey,
    pub token_program_b: Pubkey,
    pub fee_bps: u32,
}

impl CpmmPoolSpec {
    fn vault_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_a {
            Some(self.vault_a)
        } else if mint == &self.mint_b {
            Some(self.vault_b)
        } else {
            None
        }
    }

    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_a {
            Some(self.token_program_a)
        } else if mint == &self.mint_b {
            Some(self.token_program_b)
        } else {
            None
        }
    }

    /// Accounts to fetch: both vaults (reserves).
    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.vault_a, self.vault_b]
    }

    /// Decode into [`engine::Kind::Cp`] from vault balances (real SPL account decoder).
    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let res_a = pools::parse_token_amount(src.get_account(&self.vault_a)?)?;
        let res_b = pools::parse_token_amount(src.get_account(&self.vault_b)?)?;
        Some(Pool {
            id: self.pool_state,
            token_a: self.mint_a,
            token_b: self.mint_b,
            kind: engine::Kind::Cp { res_a, res_b, fee_bps: self.fee_bps },
        })
    }

    /// CPMM leg for `in_mint → out_mint`; returns the accounts (15) and the direction byte (informational).
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let plan = route::CpmmPlan {
            payer: ctx.bot,
            amm_config: self.amm_config,
            pool_state: self.pool_state,
            input_token_account: ctx.token_account(&in_mint)?,
            output_token_account: ctx.token_account(&out_mint)?,
            input_vault: self.vault_of(&in_mint)?,
            output_vault: self.vault_of(&out_mint)?,
            input_token_program: self.program_of(&in_mint)?,
            output_token_program: self.program_of(&out_mint)?,
            input_token_mint: in_mint,
            output_token_mint: out_mint,
        };
        let direction = if in_mint == self.mint_a { 0 } else { 1 };
        Some((route::cpmm_hop(&plan).accounts(), direction, VENUE_CPMM))
    }
}

/// Static (snapshot) description of a DLMM pool. Decode needs LbPair data + the active BinArray; `active_id`
/// and `active_bin_array` are a snapshot from the last LbPair read (refresh every cycle — two-phase fetch:
/// LbPair → its active BinArray + neighbours).
#[derive(Clone, Debug)]
pub struct DlmmPoolSpec {
    pub lb_pair: Pubkey,
    pub active_bin_array: Pubkey,
    pub active_id: i32,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub token_x_program: Pubkey,
    pub token_y_program: Pubkey,
    pub bitmap_extension: Option<Pubkey>,
    pub host_fee_in: Option<Pubkey>,
    /// How many BinArray accounts to pass (variable tail). In the loop: from the swap range + the pool bitmap.
    pub tail_count: u32,
}

impl DlmmPoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.token_x_mint {
            Some(self.token_x_program)
        } else if mint == &self.token_y_mint {
            Some(self.token_y_program)
        } else {
            None
        }
    }

    /// Accounts to fetch: LbPair + the active BinArray.
    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.lb_pair, self.active_bin_array]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let lb = src.get_account(&self.lb_pair)?;
        let arr = src.get_account(&self.active_bin_array)?;
        crate::dlmm_pool_from_parts(self.lb_pair, lb, arr)
    }

    /// DLMM leg for `in_mint → out_mint`. The direction determines which way the bins go: X→Y (`in`=X)
    /// lowers `active_id` (descending tail); Y→X (`in`=Y) raises it (ascending tail). Oracle and
    /// BinArray accounts computed on the fly ([`crate::route::dlmm_hop`]).
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let x_in = in_mint == self.token_x_mint; // X→Y
        let ascending = !x_in; // Y→X raises active_id
        let indexes = vp::dlmm_bin_array_indices(self.active_id, ascending, self.tail_count);
        let plan = route::DlmmPlan {
            lb_pair: self.lb_pair,
            bitmap_extension: self.bitmap_extension,
            reserve_x: self.reserve_x,
            reserve_y: self.reserve_y,
            user_token_in: ctx.token_account(&in_mint)?,
            user_token_out: ctx.token_account(&out_mint)?,
            token_x_mint: self.token_x_mint,
            token_y_mint: self.token_y_mint,
            host_fee_in: self.host_fee_in,
            user: ctx.bot,
            token_x_program: self.token_x_program,
            token_y_program: self.token_y_program,
            bin_array_indexes: indexes,
        };
        let built = route::dlmm_hop(&plan);
        let v2 = uses_token_2022(self.token_x_program, self.token_y_program);
        let accounts = if v2 { built.accounts() } else { built.accounts_v1() };
        let venue = if v2 { VENUE_DLMM } else { VENUE_DLMM_V1 };
        let direction = if x_in { 0 } else { 1 };
        Some((accounts, direction, venue))
    }
}

/// Static (snapshot) description of a Raydium CLMM pool. `current_tick`/`tick_spacing` are a snapshot from reading
/// `pool_state` (to compute the tick array tail). The CLMM fee comes from `amm_config` (not from the pool
/// account) — hence `fee_bps` in the spec.
#[derive(Clone, Debug)]
pub struct ClmmPoolSpec {
    pub pool_state: Pubkey,
    pub amm_config: Pubkey,
    pub mint_0: Pubkey,
    pub mint_1: Pubkey,
    pub vault_0: Pubkey,
    pub vault_1: Pubkey,
    pub token_program_0: Pubkey,
    pub token_program_1: Pubkey,
    pub current_tick: i32,
    pub tick_spacing: u16,
    /// Whether to add `pool_tick_array_bitmap_extension` as the first tail account (usually yes).
    pub include_bitmap_extension: bool,
    /// How many tick arrays to pass (variable tail).
    pub tail_count: u32,
    pub fee_bps: u32,
    /// Pool program: Raydium CLMM, Pancake or Byreal. PDA seeds are shared.
    pub program: Pubkey,
    /// Long-list code. Raydium `0x02`, Pancake `0x07`, Byreal `0x0a`.
    pub venue_v2: u8,
    /// Short-list code. `None` keeps the long list (Byreal, always `0x0a`).
    pub venue_v1: Option<u8>,
}

impl ClmmPoolSpec {
    fn vault_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_0 {
            Some(self.vault_0)
        } else if mint == &self.mint_1 {
            Some(self.vault_1)
        } else {
            None
        }
    }

    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_0 {
            Some(self.token_program_0)
        } else if mint == &self.mint_1 {
            Some(self.token_program_1)
        } else {
            None
        }
    }

    /// Decode needs only the `pool_state` account (pricing from sqrt-price + liquidity; single-range here).
    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.pool_state]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        crate::clmm_pool_from_account(self.pool_state, src.get_account(&self.pool_state)?, self.fee_bps)
    }

    /// CLMM leg for `in_mint → out_mint`. `a_to_b` = `in` = token0 (zero_for_one), tick falls →
    /// arrays downward (ascending=false); the reverse for one_for_zero. Observation/bitmap/arrays
    /// computed on the fly ([`crate::route::clmm_hop`]).
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let a_to_b = in_mint == self.mint_0;
        let ascending = !a_to_b;
        let starts = vp::clmm_tick_array_starts(self.current_tick, self.tick_spacing, ascending, self.tail_count);
        // Byreal always has the long list. Raydium and Pancake take the short one when both programs are SPL.
        let v2 = self.venue_v1.is_none() || uses_token_2022(self.token_program_0, self.token_program_1);
        let plan = route::ClmmPlan {
            program: self.program,
            payer: ctx.bot,
            amm_config: self.amm_config,
            pool_state: self.pool_state,
            input_token_account: ctx.token_account(&in_mint)?,
            output_token_account: ctx.token_account(&out_mint)?,
            input_vault: self.vault_of(&in_mint)?,
            output_vault: self.vault_of(&out_mint)?,
            input_vault_mint: in_mint,
            output_vault_mint: out_mint,
            include_bitmap_extension: self.include_bitmap_extension,
            bitmap_last: self.program == vp::PANCAKE && !v2,
            tick_array_starts: starts,
        };
        let built = route::clmm_hop(&plan);
        let accounts = if v2 { built.accounts() } else { built.accounts_v1() };
        let venue = if v2 { self.venue_v2 } else { self.venue_v1? };
        // Raydium v2 writes 0 for a→b: the byte does not go into the CPI data.
        // Raydium v1, Pancake (v1 and v2) and Byreal write 1 when the input is mint 0.
        let direction = if self.program == vp::CLMM && v2 {
            if a_to_b { 0 } else { 1 }
        } else if a_to_b {
            1
        } else {
            0
        };
        Some((accounts, direction, venue))
    }
}

/// Static description of an Orca Whirlpool pool. `tick_current` and `tick_spacing` are a snapshot from the pool account.
/// Tick arrays are computed by [`keys::venue_pda`]: v1 three in the swap direction, v2 a window of five around the current one.
#[derive(Clone, Debug)]
pub struct WhirlpoolPoolSpec {
    pub whirlpool: Pubkey,
    pub token_program_a: Pubkey,
    pub token_program_b: Pubkey,
    pub token_mint_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_vault_b: Pubkey,
    pub tick_current: i32,
    pub tick_spacing: u16,
}

impl WhirlpoolPoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.token_mint_a {
            Some(self.token_program_a)
        } else if mint == &self.token_mint_b {
            Some(self.token_program_b)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.whirlpool]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        crate::whirlpool_pool_from_account(self.whirlpool, src.get_account(&self.whirlpool)?)
    }

    /// Whirlpool leg. The account layout covers both sides (owner/vault A and B).
    /// The route direction byte is the `a_to_b` byte in the CPI: `in` = mint_a → 1.
    /// Oracle and tick arrays computed ([`crate::route::whirlpool_hop`]).
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, _out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let a_to_b = in_mint == self.token_mint_a;
        let v2 = uses_token_2022(self.token_program_a, self.token_program_b);
        let (core, supplemental) = if v2 {
            let (core, supp) = vp::whirlpool_v2_window(self.tick_current, self.tick_spacing);
            (core, supp)
        } else {
            (vp::whirlpool_tick_array_starts(self.tick_current, self.tick_spacing, !a_to_b, 3), Vec::new())
        };
        let tick = |start: i32| vp::whirlpool_tick_array(&self.whirlpool, start);
        let plan = route::WhirlpoolPlan {
            token_program_a: self.token_program_a,
            token_program_b: self.token_program_b,
            token_authority: ctx.bot,
            whirlpool: self.whirlpool,
            token_mint_a: self.token_mint_a,
            token_mint_b: self.token_mint_b,
            token_owner_a: ctx.token_account(&self.token_mint_a)?,
            token_vault_a: self.token_vault_a,
            token_owner_b: ctx.token_account(&self.token_mint_b)?,
            token_vault_b: self.token_vault_b,
            tick_array_0: tick(core[0]),
            tick_array_1: tick(core[1]),
            tick_array_2: tick(core[2]),
            supplemental_tick_arrays: supplemental.iter().copied().map(tick).collect(),
        };
        let built = route::whirlpool_hop(&plan);
        let accounts = if v2 { built.accounts() } else { built.accounts_v1() };
        let venue = if v2 { VENUE_WHIRLPOOL } else { VENUE_WHIRLPOOL_V1 };
        let direction = if a_to_b { 1 } else { 0 };
        Some((accounts, direction, venue))
    }
}

/// Static description of a Meteora DAMM v2 pool (constant-product — reserves from vault balances, like CPMM).
/// No derived per-pool PDAs (POOL_AUTHORITY/EVENT_AUTHORITY are program constants); `referral`
/// = `None` → program sentinel.
#[derive(Clone, Debug)]
pub struct DammV2PoolSpec {
    pub pool: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub token_program_a: Pubkey,
    pub token_program_b: Pubkey,
    pub referral: Option<Pubkey>,
    pub fee_bps: u32,
}

impl DammV2PoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_a {
            Some(self.token_program_a)
        } else if mint == &self.mint_b {
            Some(self.token_program_b)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.vault_a, self.vault_b]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let res_a = pools::parse_token_amount(src.get_account(&self.vault_a)?)?;
        let res_b = pools::parse_token_amount(src.get_account(&self.vault_b)?)?;
        Some(Pool {
            id: self.pool,
            token_a: self.mint_a,
            token_b: self.mint_b,
            kind: engine::Kind::Cp { res_a, res_b, fee_bps: self.fee_bps },
        })
    }

    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let plan = route::DammV2Plan {
            pool: self.pool,
            input_token_account: ctx.token_account(&in_mint)?,
            output_token_account: ctx.token_account(&out_mint)?,
            token_a_vault: self.vault_a,
            token_b_vault: self.vault_b,
            token_a_mint: self.mint_a,
            token_b_mint: self.mint_b,
            payer: ctx.bot,
            token_a_program: self.token_program_a,
            token_b_program: self.token_program_b,
            referral: self.referral,
        };
        let direction = if in_mint == self.mint_a { 0 } else { 1 };
        Some((route::damm_v2_hop(&plan).accounts(), direction, VENUE_DAMM_V2))
    }
}

/// Static description of a Pump AMM pool (constant-product). Accounts resolved on the fly by the operator/
/// resolver (fee recipient, creator vault, 4 derived accounts) are kept as a cycle snapshot.
///
/// **Model limitation:** our `encode_swap` handles only Pump **sell** (exact-in, base→quote);
/// **buy** (exact-out) is outside the model. So a Pump pool can only be a **sell** leg (for base
/// = WSOL = quote: leg T→WSOL). `hop` returns `None` for the buy direction.
#[derive(Clone, Debug)]
pub struct PumpPoolSpec {
    pub pool: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    // Resolved on the fly (cycle snapshot):
    pub fee_recipient: Pubkey,
    pub fee_recipient_ata: Pubkey,
    pub creator_vault_ata: Pubkey,
    pub creator_vault_authority: Pubkey,
    pub extra: [AccountMeta; 4],
    pub fee_bps: u32,
}

impl PumpPoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.base_mint {
            Some(self.base_token_program)
        } else if mint == &self.quote_mint {
            Some(self.quote_token_program)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.base_vault, self.quote_vault]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let res_base = pools::parse_token_amount(src.get_account(&self.base_vault)?)?;
        let res_quote = pools::parse_token_amount(src.get_account(&self.quote_vault)?)?;
        Some(Pool {
            id: self.pool,
            token_a: self.base_mint,
            token_b: self.quote_mint,
            kind: engine::Kind::Cp { res_a: res_base, res_b: res_quote, fee_bps: self.fee_bps },
        })
    }

    /// Selling base→quote is direction 0 (`sell`). Buying quote→base is direction 1
    /// (`buy_exact_quote_in`). The leg accounts are the same.
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let direction = if in_mint == self.base_mint && out_mint == self.quote_mint {
            0
        } else if in_mint == self.quote_mint && out_mint == self.base_mint {
            1
        } else {
            return None;
        };
        let plan = route::PumpPlan {
            pool: self.pool,
            base_mint: self.base_mint,
            quote_mint: self.quote_mint,
            base_vault: self.base_vault,
            quote_vault: self.quote_vault,
            user: ctx.bot,
            user_base_ata: ctx.token_account(&self.base_mint)?,
            user_quote_ata: ctx.token_account(&self.quote_mint)?,
            fee_recipient: self.fee_recipient,
            fee_recipient_ata: self.fee_recipient_ata,
            base_token_program: self.base_token_program,
            quote_token_program: self.quote_token_program,
            creator_vault_ata: self.creator_vault_ata,
            creator_vault_authority: self.creator_vault_authority,
            extra: self.extra.clone(),
        };
        Some((route::pump_hop(&plan).accounts(), direction, VENUE_PUMP))
    }
}

/// Raydium AMM v4. Reserves are vault balances (constant-product). The fee is in the spec,
/// because the `AmmInfo` account is not decoded here. `coin` is the vault at position 3 of the V2 list.
#[derive(Clone, Debug)]
pub struct RayV4PoolSpec {
    pub amm: Pubkey,
    pub coin_mint: Pubkey,
    pub pc_mint: Pubkey,
    pub coin_vault: Pubkey,
    pub pc_vault: Pubkey,
    pub fee_bps: u32,
}

impl RayV4PoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.coin_mint || mint == &self.pc_mint {
            Some(TOKEN)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.coin_vault, self.pc_vault]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let res_coin = pools::parse_token_amount(src.get_account(&self.coin_vault)?)?;
        let res_pc = pools::parse_token_amount(src.get_account(&self.pc_vault)?)?;
        Some(Pool {
            id: self.amm,
            token_a: self.coin_mint,
            token_b: self.pc_mint,
            kind: engine::Kind::Cp { res_a: res_coin, res_b: res_pc, fee_bps: self.fee_bps },
        })
    }

    /// `SwapBaseInV2`: the source is the input account, the destination is the output account. The direction byte
    /// does not go into the CPI data. 0 means input in coin.
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let coin_in = if in_mint == self.coin_mint && out_mint == self.pc_mint {
            true
        } else if in_mint == self.pc_mint && out_mint == self.coin_mint {
            false
        } else {
            return None;
        };
        let hop = ray_v4::RayV4Hop {
            amm: self.amm,
            coin_vault: self.coin_vault,
            pc_vault: self.pc_vault,
            user_source: ctx.token_account(&in_mint)?,
            user_destination: ctx.token_account(&out_mint)?,
            user_owner: ctx.bot,
        };
        let direction = if coin_in { 0 } else { 1 };
        Some((hop.accounts(), direction, VENUE_RAY_V4))
    }
}

/// Meteora DAMM v1. A side's reserve is `token_vault_balance * pool_lp_balance / lp_supply`,
/// not the raw vault balance. The fee is in the spec. The protocol fee account depends on the input side.
#[derive(Clone, Debug)]
pub struct DammV1PoolSpec {
    pub pool: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_token_vault: Pubkey,
    pub b_token_vault: Pubkey,
    pub a_lp_mint: Pubkey,
    pub b_lp_mint: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    /// Protocol fee used when the input is `mint_a`.
    pub protocol_fee_a: Pubkey,
    /// Protocol fee used when the input is `mint_b`.
    pub protocol_fee_b: Pubkey,
    pub fee_bps: u32,
}

impl DammV1PoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.mint_a || mint == &self.mint_b {
            Some(TOKEN)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![
            self.a_token_vault,
            self.b_token_vault,
            self.a_vault_lp,
            self.b_vault_lp,
            self.a_lp_mint,
            self.b_lp_mint,
        ]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let token = |key: &Pubkey| pools::parse_token_amount(src.get_account(key)?);
        let supply = |key: &Pubkey| pools::parse_mint_supply(src.get_account(key)?);
        let res_a = pools::vault_share(token(&self.a_token_vault)?, token(&self.a_vault_lp)?, supply(&self.a_lp_mint)?)?;
        let res_b = pools::vault_share(token(&self.b_token_vault)?, token(&self.b_vault_lp)?, supply(&self.b_lp_mint)?)?;
        Some(Pool {
            id: self.pool,
            token_a: self.mint_a,
            token_b: self.mint_b,
            kind: engine::Kind::Cp { res_a, res_b, fee_bps: self.fee_bps },
        })
    }

    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let a_in = in_mint == self.mint_a && out_mint == self.mint_b;
        let b_in = in_mint == self.mint_b && out_mint == self.mint_a;
        if !a_in && !b_in {
            return None;
        }
        let hop = damm_v1::DammV1Hop {
            pool: self.pool,
            user_source: ctx.token_account(&in_mint)?,
            user_destination: ctx.token_account(&out_mint)?,
            a_vault: self.a_vault,
            b_vault: self.b_vault,
            a_token_vault: self.a_token_vault,
            b_token_vault: self.b_token_vault,
            a_lp_mint: self.a_lp_mint,
            b_lp_mint: self.b_lp_mint,
            a_vault_lp: self.a_vault_lp,
            b_vault_lp: self.b_vault_lp,
            protocol_fee: if a_in { self.protocol_fee_a } else { self.protocol_fee_b },
            user: ctx.bot,
        };
        let direction = if a_in { 0 } else { 1 };
        Some((hop.accounts(), direction, VENUE_DAMM_V1))
    }
}

/// Manifest. The book is in the market account. At slot 0 nothing expires. A global order
/// ends the walk down the book: the global deposit balance is not modeled here.
/// Direction 1 means input in base (`is_base_in`).
#[derive(Clone, Debug)]
pub struct ManifestPoolSpec {
    pub market: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub token_program_base: Pubkey,
    pub token_program_quote: Pubkey,
}

impl ManifestPoolSpec {
    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        if mint == &self.base_mint {
            Some(self.token_program_base)
        } else if mint == &self.quote_mint {
            Some(self.token_program_quote)
        } else {
            None
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        vec![self.market]
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        let data = src.get_account(&self.market)?;
        let (base, quote, book) = sim::book::decode(data, 0)?;
        let (base_vault, quote_vault) = sim::book::vaults(data)?;
        if base != self.base_mint
            || quote != self.quote_mint
            || base_vault != self.base_vault
            || quote_vault != self.quote_vault
        {
            return None;
        }
        Some(Pool {
            id: self.market,
            token_a: self.base_mint,
            token_b: self.quote_mint,
            kind: engine::Kind::Book { bids: book.bids, asks: book.asks },
        })
    }

    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, u8, u8)> {
        let base_in = in_mint == self.base_mint && out_mint == self.quote_mint;
        let quote_in = in_mint == self.quote_mint && out_mint == self.base_mint;
        if !base_in && !quote_in {
            return None;
        }
        let hop = route::manifest_hop(&route::ManifestPlan {
            payer: ctx.bot,
            owner: ctx.bot,
            market: self.market,
            trader_base: ctx.token_account(&self.base_mint)?,
            trader_quote: ctx.token_account(&self.quote_mint)?,
            base_vault: self.base_vault,
            quote_vault: self.quote_vault,
            token_program_base: self.token_program_base,
            base_mint: self.base_mint,
            token_program_quote: self.token_program_quote,
            quote_mint: self.quote_mint,
        });
        let direction = if base_in { 1 } else { 0 };
        Some((hop.accounts(), direction, VENUE_MANIFEST))
    }
}

/// Uniform pool description — one variant per venue. Adding an exchange = a new variant + its `*_hop`.
#[derive(Clone, Debug)]
pub enum PoolSpec {
    Cpmm(CpmmPoolSpec),
    Dlmm(DlmmPoolSpec),
    Clmm(ClmmPoolSpec),
    Whirlpool(WhirlpoolPoolSpec),
    DammV2(DammV2PoolSpec),
    Pump(PumpPoolSpec),
    RayV4(RayV4PoolSpec),
    DammV1(DammV1PoolSpec),
    Manifest(ManifestPoolSpec),
}

impl PoolSpec {
    /// Pool key = `Pool::id` (to match opportunities from the engine).
    pub fn id(&self) -> Pubkey {
        match self {
            PoolSpec::Cpmm(s) => s.pool_state,
            PoolSpec::Dlmm(s) => s.lb_pair,
            PoolSpec::Clmm(s) => s.pool_state,
            PoolSpec::Whirlpool(s) => s.whirlpool,
            PoolSpec::DammV2(s) => s.pool,
            PoolSpec::Pump(s) => s.pool,
            PoolSpec::RayV4(s) => s.amm,
            PoolSpec::DammV1(s) => s.pool,
            PoolSpec::Manifest(s) => s.market,
        }
    }

    fn account_keys(&self) -> Vec<Pubkey> {
        match self {
            PoolSpec::Cpmm(s) => s.account_keys(),
            PoolSpec::Dlmm(s) => s.account_keys(),
            PoolSpec::Clmm(s) => s.account_keys(),
            PoolSpec::Whirlpool(s) => s.account_keys(),
            PoolSpec::DammV2(s) => s.account_keys(),
            PoolSpec::Pump(s) => s.account_keys(),
            PoolSpec::RayV4(s) => s.account_keys(),
            PoolSpec::DammV1(s) => s.account_keys(),
            PoolSpec::Manifest(s) => s.account_keys(),
        }
    }

    fn decode(&self, src: &dyn AccountSource) -> Option<Pool> {
        match self {
            PoolSpec::Cpmm(s) => s.decode(src),
            PoolSpec::Dlmm(s) => s.decode(src),
            PoolSpec::Clmm(s) => s.decode(src),
            PoolSpec::Whirlpool(s) => s.decode(src),
            PoolSpec::DammV2(s) => s.decode(src),
            PoolSpec::Pump(s) => s.decode(src),
            PoolSpec::RayV4(s) => s.decode(src),
            PoolSpec::DammV1(s) => s.decode(src),
            PoolSpec::Manifest(s) => s.decode(src),
        }
    }

    fn program_of(&self, mint: &Pubkey) -> Option<Pubkey> {
        match self {
            PoolSpec::Cpmm(s) => s.program_of(mint),
            PoolSpec::Dlmm(s) => s.program_of(mint),
            PoolSpec::Clmm(s) => s.program_of(mint),
            PoolSpec::Whirlpool(s) => s.program_of(mint),
            PoolSpec::DammV2(s) => s.program_of(mint),
            PoolSpec::Pump(s) => s.program_of(mint),
            PoolSpec::RayV4(s) => s.program_of(mint),
            PoolSpec::DammV1(s) => s.program_of(mint),
            PoolSpec::Manifest(s) => s.program_of(mint),
        }
    }

    /// Builds the CPI leg for `in_mint → out_mint` and its metadata (`PlanHop`). Per-venue dispatch.
    fn hop(&self, ctx: &BotCtx, in_mint: Pubkey, out_mint: Pubkey) -> Option<(Vec<AccountMeta>, PlanHop)> {
        let (accounts, direction, venue) = match self {
            PoolSpec::Cpmm(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::Dlmm(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::Clmm(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::Whirlpool(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::DammV2(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::Pump(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::RayV4(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::DammV1(s) => s.hop(ctx, in_mint, out_mint)?,
            PoolSpec::Manifest(s) => s.hop(ctx, in_mint, out_mint)?,
        };
        let n_accounts = accounts.len() as u8;
        Some((accounts, PlanHop { venue, direction, n_accounts, token_account_size: 0 }))
    }
}

/// Multi-venue pool registry + bot context — implements BOTH runner seams. A route can mix
/// venues: for each leg the planner picks the pool variant by its `id` and builds the right leg.
#[derive(Clone, Debug)]
pub struct Registry {
    pub ctx: BotCtx,
    pub pools: Vec<PoolSpec>,
}

impl Registry {
    pub fn new(ctx: BotCtx, pools: Vec<PoolSpec>) -> Self {
        Self { ctx, pools }
    }

    fn spec(&self, pool_id: &Pubkey) -> Option<&PoolSpec> {
        self.pools.iter().find(|p| &p.id() == pool_id)
    }

    /// One leg + the intermediate triple for the output token (when there is an intermediate).
    fn hop_and_inter(
        &self,
        spec: &PoolSpec,
        in_mint: Pubkey,
        out_mint: Pubkey,
        intermediate_mint: Option<Pubkey>,
    ) -> Option<(Vec<AccountMeta>, PlanHop, Option<[AccountMeta; 3]>)> {
        let (accounts, ph) = spec.hop(&self.ctx, in_mint, out_mint)?;
        let inter = match intermediate_mint {
            Some(m) => Some(intermediate_triple(self.ctx.token_account(&m)?, m, spec.program_of(&m)?)),
            None => None,
        };
        Some((accounts, ph, inter))
    }
}

impl PoolBuilder for Registry {
    fn account_keys(&self) -> Vec<Pubkey> {
        self.pools.iter().flat_map(|p| p.account_keys()).collect()
    }

    fn build_pools(&self, src: &dyn AccountSource) -> Vec<Pool> {
        self.pools.iter().filter_map(|p| p.decode(src)).collect()
    }
}

impl RoutePlanner for Registry {
    /// 2-leg route `base →(buy) T →(sell) base`. Each leg can come from a different venue.
    fn plan_two_hop(&self, opp: &Opportunity) -> Option<PlannedRoute> {
        let buy = self.spec(&opp.buy_pool)?;
        let sell = self.spec(&opp.sell_pool)?;
        let (base, t) = (self.ctx.base_mint, opp.token);

        // We take the intermediate triple of T from the buy pool (T is its output token).
        let (hop0, ph0, inter_t) = self.hop_and_inter(buy, base, t, Some(t))?;
        let (hop1, ph1, _) = self.hop_and_inter(sell, t, base, None)?;

        Some(PlannedRoute {
            intermediates: vec![inter_t?],
            hops: vec![hop0, hop1],
            plan_hops: vec![ph0, ph1],
        })
    }

    /// 3-leg route `base →(p0) T →(p1) U →(p2) base`. Any mix of venues.
    fn plan_three_hop(&self, opp: &TriOpportunity) -> Option<PlannedRoute> {
        let p0 = self.spec(&opp.pools[0])?;
        let p1 = self.spec(&opp.pools[1])?;
        let p2 = self.spec(&opp.pools[2])?;
        let (base, t, u) = (self.ctx.base_mint, opp.tokens[0], opp.tokens[1]);

        let (hop0, ph0, inter_t) = self.hop_and_inter(p0, base, t, Some(t))?;
        let (hop1, ph1, inter_u) = self.hop_and_inter(p1, t, u, Some(u))?;
        let (hop2, ph2, _) = self.hop_and_inter(p2, u, base, None)?;

        Some(PlannedRoute {
            intermediates: vec![inter_t?, inter_u?],
            hops: vec![hop0, hop1, hop2],
            plan_hops: vec![ph0, ph1, ph2],
        })
    }

    /// 4- or 5-leg cycle: `base → intermediate tokens → base`. The last leg adds no triple.
    fn plan_cycle(&self, opp: &engine::CycleOpportunity) -> Option<PlannedRoute> {
        let n = opp.pools.len();
        if !(4..=5).contains(&n) || opp.tokens.len() + 1 != n {
            return None;
        }
        let mut mints = Vec::with_capacity(n + 1);
        mints.push(self.ctx.base_mint);
        mints.extend_from_slice(&opp.tokens);
        mints.push(self.ctx.base_mint);

        let mut intermediates = Vec::with_capacity(n - 1);
        let mut hops = Vec::with_capacity(n);
        let mut plan_hops = Vec::with_capacity(n);
        for i in 0..n {
            let spec = self.spec(&opp.pools[i])?;
            let out = mints[i + 1];
            let inter_mint = if i + 1 == n { None } else { Some(out) };
            let (hop, ph, inter) = self.hop_and_inter(spec, mints[i], out, inter_mint)?;
            hops.push(hop);
            plan_hops.push(ph);
            if let Some(triple) = inter {
                intermediates.push(triple);
            }
        }
        if intermediates.len() + 1 != hops.len() {
            return None;
        }
        Some(PlannedRoute { intermediates, hops, plan_hops })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{build_engine, DryRunSubmitter, Runner, RunnerConfig, Submitter};
    use crate::MapSource;
    use engine::TriOpportunity;
    use send::bid;
    use send::route::{clmm, cpmm, damm_v1, damm_v2, dlmm, manifest, pump_amm, whirlpool, MEMO, TOKEN, TOKEN_2022};

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// SPL token account (165 B) with the balance at offset 64 — like a real vault.
    fn token_account(amount: u64) -> Vec<u8> {
        let mut a = vec![0u8; 165];
        a[64..72].copy_from_slice(&amount.to_le_bytes());
        a
    }

    const BASE: u8 = 1; // WSOL / Y
    const T: u8 = 2; // intermediate token / X

    fn ctx() -> BotCtx {
        BotCtx {
            bot: id(100),
            base_mint: id(BASE),
            token_accounts: vec![(id(BASE), id(200)), (id(T), id(201))],
        }
    }

    fn cpmm(pool: u8, vbase: u8, vtok: u8) -> CpmmPoolSpec {
        CpmmPoolSpec {
            pool_state: id(pool),
            amm_config: id(pool + 50),
            mint_a: id(BASE),
            mint_b: id(T),
            vault_a: id(vbase),
            vault_b: id(vtok),
            token_program_a: TOKEN,
            token_program_b: TOKEN,
            fee_bps: 30,
        }
    }

    fn dlmm_spec(lb: u8, active_bin: u8) -> DlmmPoolSpec {
        DlmmPoolSpec {
            lb_pair: id(lb),
            active_bin_array: id(active_bin),
            active_id: -750, // → index -11
            reserve_x: id(lb + 40),
            reserve_y: id(lb + 41),
            token_x_mint: id(T), // X = token
            token_y_mint: id(BASE), // Y = base
            token_x_program: TOKEN,
            token_y_program: TOKEN,
            bitmap_extension: None,
            host_fee_in: None,
            tail_count: 1,
        }
    }

    /// MapSource with balances: pool A base 1e6/T 2e6 (T cheap), pool B base 2e6/T 1e6 (T expensive).
    fn cpmm_source() -> MapSource {
        let mut s = MapSource::new();
        s.insert(id(20), token_account(1_000_000));
        s.insert(id(21), token_account(2_000_000));
        s.insert(id(30), token_account(2_000_000));
        s.insert(id(31), token_account(1_000_000));
        s
    }

    /// Active BinArray built as in the `feed` decoder test (active_id -240 → position 40).
    fn dlmm_active_array() -> Vec<u8> {
        let active_bin = hex(include_str!("testdata/dlmm_active_bin.hex"));
        let mut arr = vec![0u8; 56 + 70 * 144];
        arr[8..16].copy_from_slice(&(-4i64).to_le_bytes());
        let off = 56 + 40 * 144;
        arr[off..off + 144].copy_from_slice(&active_bin);
        arr
    }

    fn cfg() -> RunnerConfig {
        RunnerConfig {
            base: id(BASE),
            bot: id(100),
            arb_program: id(240),
            state_pda: id(241),
            base_ata: id(200),
            native_bridge_pda: id(242),
            wsol_mint: id(BASE),
            rent_sysvar: id(243),
            max_input: 500_000,
            min_profit: 0,
            sol_price: 0,
            min_margin: 1,
            cu_limit: 127_000,
            cu_price_micro: 61_825,
            loaded_accounts_data_bytes: 131_072,
            auction_frac_bps: bid::OBSERVED_AUCTION_FRAC_BPS,
            auction_min_e: 0,
            auction_lane: 2,
            tip_account: Some(id(250)),
            create_intermediate_accounts: true,
            use_durable_nonce: true,
            nonce_account: id(251),
            nonce_authority: id(100),
            alts: Vec::new(),
            nonce_pair: None,
            copy_index: 0,
        }
    }

    #[test]
    fn pool_builder_decodes_mixed_venues() {
        // Registry with a CPMM pool (decoded from vaults) and a DLMM pool (decoded from LbPair+BinArray, real fixture).
        let reg = Registry::new(
            ctx(),
            vec![
                PoolSpec::Cpmm(cpmm(10, 20, 21)),
                PoolSpec::Dlmm(DlmmPoolSpec { lb_pair: id(11), active_bin_array: id(22), ..dlmm_spec(11, 22) }),
            ],
        );
        let mut src = cpmm_source();
        src.insert(id(11), hex(include_str!("testdata/lbpair_dlmm.hex")));
        src.insert(id(22), dlmm_active_array());

        // account_keys mixes both venues: [vaultA, vaultB, lb_pair, active_bin_array].
        assert_eq!(reg.account_keys(), vec![id(20), id(21), id(11), id(22)]);

        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 2);
        assert!(matches!(built[0].kind, engine::Kind::Cp { .. }), "pool 0 = CPMM → Cp");
        assert!(matches!(built[1].kind, engine::Kind::Dlmm { .. }), "pool 1 = DLMM → Dlmm");
        assert_eq!(built[0].id, id(10));
        assert_eq!(built[1].id, id(11));
    }

    #[test]
    fn plan_mixed_route_dispatches_per_venue() {
        // Mixed route: BUY on CPMM, SELL on DLMM — one transaction, two venues.
        let reg = Registry::new(
            ctx(),
            vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Dlmm(dlmm_spec(11, 22))],
        );
        let opp = Opportunity {
            token: id(T),
            buy_pool: id(10),  // CPMM
            sell_pool: id(11), // DLMM
            amount_in: 100_000,
            profit: 80_000,
        };
        let planned = reg.plan_two_hop(&opp).expect("mixed route");
        assert_eq!(planned.hops.len(), 2);
        assert_eq!(planned.intermediates.len(), 1);

        // Leg 0 = CPMM (15 accounts), leg 1 = DLMM v1 (17 accounts, SPL without MEMO).
        assert_eq!(planned.hops[0].len(), 15);
        assert_eq!(planned.hops[0][0].pubkey, cpmm::PROGRAM, "leg 0 → CPMM program");
        assert_eq!(planned.hops[1].len(), 17);
        assert_eq!(planned.hops[1][0].pubkey, dlmm::PROGRAM, "leg 1 → DLMM program");

        // Metadata: venue CPMM then DLMM v1 (both token programs are SPL).
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_CPMM, direction: 0, n_accounts: 15, token_account_size: 0 });
        assert_eq!(planned.plan_hops[1], PlanHop { venue: VENUE_DLMM_V1, direction: 0, n_accounts: 17, token_account_size: 0 });

        // Intermediate account T from the buy pool (CPMM): token account + mint T + program T.
        assert_eq!(planned.intermediates[0][0].pubkey, id(201));
        assert_eq!(planned.intermediates[0][1].pubkey, id(T));
        assert_eq!(planned.intermediates[0][2].pubkey, TOKEN);
    }

    #[test]
    fn plan_all_cpmm_route() {
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Cpmm(cpmm(11, 30, 31))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 50_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        assert!(planned.hops.iter().all(|h| h.len() == 15));
        assert!(planned.plan_hops.iter().all(|h| h.venue == VENUE_CPMM));
        // Hop 0 (base→T): input = base ATA, output = T account.
        assert_eq!(planned.hops[0][5].pubkey, id(200));
        assert_eq!(planned.hops[0][6].pubkey, id(201));
    }

    #[test]
    fn dlmm_hop_computes_oracle_and_variable_tail() {
        let reg = Registry::new(ctx(), vec![PoolSpec::Dlmm(dlmm_spec(10, 20)), PoolSpec::Dlmm(dlmm_spec(11, 21))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();

        let h0 = &planned.hops[0]; // base→T = Y→X (buy X), SPL → swap without MEMO
        assert_eq!(h0.len(), 17);
        assert_eq!(h0[0].pubkey, dlmm::PROGRAM);
        assert_eq!(h0[14].pubkey, dlmm::EVENT_AUTHORITY);
        assert_eq!(h0[15].pubkey, dlmm::PROGRAM);
        // Oracle and BinArray COMPUTED from pool state (not copied).
        assert_eq!(h0[9].pubkey, vp::dlmm_oracle(&id(10)));
        let idx = vp::dlmm_bin_array_indices(-750, true, 1)[0]; // Y→X → ascending
        assert_eq!(h0[16].pubkey, vp::dlmm_bin_array(&id(10), idx));
        assert!(h0[16].is_writable);
        assert!(h0[11].is_signer, "user (bot) signs");
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_DLMM_V1, direction: 1, n_accounts: 17, token_account_size: 0 });
    }

    #[test]
    fn dlmm_token2022_keeps_swap2() {
        let mut spec = dlmm_spec(10, 20);
        spec.token_y_program = TOKEN_2022;
        let reg = Registry::new(ctx(), vec![PoolSpec::Dlmm(spec), PoolSpec::Dlmm(dlmm_spec(11, 21))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h0 = &planned.hops[0];
        assert_eq!(h0.len(), 18);
        assert_eq!(h0[14].pubkey, MEMO);
        assert_eq!(h0[15].pubkey, dlmm::EVENT_AUTHORITY);
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_DLMM, direction: 1, n_accounts: 18, token_account_size: 0 });
        assert_eq!(planned.plan_hops[1].venue, VENUE_DLMM_V1);
    }

    #[test]
    fn dlmm_variable_tail_scales_with_count() {
        let mut s = dlmm_spec(10, 20);
        s.tail_count = 2;
        let reg = Registry::new(ctx(), vec![PoolSpec::Dlmm(s), PoolSpec::Dlmm(dlmm_spec(11, 21))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        assert_eq!(planned.hops[0].len(), 18, "16 core v1 + 2 BinArray");
        let idxs = vp::dlmm_bin_array_indices(-750, true, 2);
        assert_eq!(planned.hops[0][16].pubkey, vp::dlmm_bin_array(&id(10), idxs[0]));
        assert_eq!(planned.hops[0][17].pubkey, vp::dlmm_bin_array(&id(10), idxs[1]));
        assert_eq!(planned.plan_hops[0].n_accounts, 18);
        assert_eq!(planned.plan_hops[0].venue, VENUE_DLMM_V1);
    }

    #[test]
    fn registry_drives_runner_end_to_end() {
        // Full path without a network: registry (2 CPMM pools) → build_engine → tick → package → submitter.
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Cpmm(cpmm(11, 30, 31))]);
        let cfg = cfg();
        let engine = build_engine(&cfg, &reg, &cpmm_source());
        let nonce = keys::durable_nonce(&id(222));
        let pkg = Runner { cfg: &cfg, planner: &reg }.tick(&engine, nonce).expect("package");

        assert!(pkg.expected_gross > 0);
        assert_eq!(pkg.message[0], 0x80, "v0 message");
        assert_eq!(pkg.message[1], 1, "one signature (bot = payer = nonce authority)");
        assert!(pkg.message.windows(32).any(|w| w == nonce), "nonce in recent_blockhash");

        let mut sub = DryRunSubmitter::default();
        if pkg.worth_sending(cfg.min_margin) {
            sub.submit(pkg);
        }
        assert_eq!(sub.packages.len(), 1);
    }

    #[test]
    fn unknown_pool_yields_no_route() {
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(99), amount_in: 100_000, profit: 50_000 };
        assert!(reg.plan_two_hop(&opp).is_none());
    }

    const U: u8 = 3; // second intermediate token (3-leg routes)

    fn clmm(pool: u8, m0: u8, m1: u8, v0: u8, v1: u8) -> ClmmPoolSpec {
        ClmmPoolSpec {
            pool_state: id(pool),
            amm_config: id(pool + 50),
            mint_0: id(m0),
            mint_1: id(m1),
            vault_0: id(v0),
            vault_1: id(v1),
            token_program_0: TOKEN,
            token_program_1: TOKEN,
            current_tick: -55000,
            tick_spacing: 10,
            include_bitmap_extension: true,
            tail_count: 1,
            fee_bps: 25,
            program: vp::CLMM,
            venue_v2: VENUE_CLMM,
            venue_v1: Some(VENUE_CLMM_V1),
        }
    }

    fn whirlpool(wp: u8, ma: u8, mb: u8) -> WhirlpoolPoolSpec {
        WhirlpoolPoolSpec {
            whirlpool: id(wp),
            token_program_a: TOKEN,
            token_program_b: TOKEN,
            token_mint_a: id(ma),
            token_mint_b: id(mb),
            token_vault_a: id(wp + 40),
            token_vault_b: id(wp + 41),
            tick_current: 100,
            tick_spacing: 8,
        }
    }

    #[test]
    fn clmm_decode_and_hop() {
        // Decode from a real Raydium CLMM pool account.
        let mut src = MapSource::new();
        src.insert(id(10), hex(include_str!("testdata/clmm_ray.hex")));
        let reg = Registry::new(ctx(), vec![PoolSpec::Clmm(clmm(10, BASE, T, 20, 21)), PoolSpec::Clmm(clmm(11, BASE, T, 30, 31))]);
        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 1, "only pool 10 has data in the source");
        assert!(matches!(built[0].kind, engine::Kind::Clmm { .. }));

        // Structure of leg base→T (= token0→token1, a_to_b): observation/bitmap/array computed.
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 12, "10 core v1 + bitmap + 1 tick array");
        assert_eq!(h[0].pubkey, clmm::PROGRAM);
        assert!(h[1].is_signer, "payer = bot");
        assert_eq!(h[8].pubkey, vp::clmm_observation(&id(10)), "observation computed");
        assert_eq!(h[9].pubkey, TOKEN);
        assert_eq!(h[10].pubkey, vp::clmm_bitmap_extension(&id(10)), "bitmap extension at the head of the tail");
        let start = vp::clmm_tick_array_starts(-55000, 10, false, 1)[0]; // base=mint0 → a_to_b → descending
        assert_eq!(h[11].pubkey, vp::clmm_tick_array(&id(10), start), "tick array computed from state");
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_CLMM_V1, direction: 1, n_accounts: 12, token_account_size: 0 });
    }

    #[test]
    fn clmm_token2022_keeps_swap_v2_and_direction_zero() {
        let mut spec = clmm(10, BASE, T, 20, 21);
        spec.token_program_1 = TOKEN_2022;
        let reg = Registry::new(ctx(), vec![PoolSpec::Clmm(spec), PoolSpec::Clmm(clmm(11, BASE, T, 30, 31))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 16);
        assert_eq!(h[9].pubkey, TOKEN);
        assert_eq!(h[10].pubkey, TOKEN_2022);
        assert_eq!(h[11].pubkey, MEMO);
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_CLMM, direction: 0, n_accounts: 16, token_account_size: 0 });
        assert_eq!(planned.plan_hops[1].venue, VENUE_CLMM_V1);
        // The second leg is T→base, i.e. not a→b, so 0x24 gets direction 0 (sqrt MAX−1).
        assert_eq!(planned.plan_hops[1].direction, 0);
    }

    #[test]
    fn pancake_v1_bitmap_is_last_and_direction_follows_mint0() {
        let mut buy = clmm(10, BASE, T, 20, 21);
        buy.program = vp::PANCAKE;
        buy.venue_v2 = VENUE_PANCAKE;
        buy.venue_v1 = Some(VENUE_PANCAKE_V1);
        let mut sell = clmm(11, T, BASE, 30, 31);
        sell.program = vp::PANCAKE;
        sell.venue_v2 = VENUE_PANCAKE;
        sell.venue_v1 = Some(VENUE_PANCAKE_V1);
        let reg = Registry::new(ctx(), vec![PoolSpec::Clmm(buy), PoolSpec::Clmm(sell)]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 12);
        assert_eq!(h[0].pubkey, vp::PANCAKE);
        let start = vp::clmm_tick_array_starts(-55000, 10, false, 1)[0];
        assert_eq!(h[10].pubkey, vp::clmm_tick_array_of(&vp::PANCAKE, &id(10), start));
        assert_eq!(h[11].pubkey, vp::clmm_bitmap_extension_of(&vp::PANCAKE, &id(10)));
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_PANCAKE_V1, direction: 1, n_accounts: 12, token_account_size: 0 });
        assert_eq!(planned.plan_hops[1].direction, 1, "T is mint0 of the sell pool");
    }

    #[test]
    fn pancake_v2_keeps_direction_one_when_input_is_mint0() {
        let mut buy = clmm(10, BASE, T, 20, 21);
        buy.program = vp::PANCAKE;
        buy.venue_v2 = VENUE_PANCAKE;
        buy.venue_v1 = Some(VENUE_PANCAKE_V1);
        buy.token_program_1 = TOKEN_2022;
        let reg = Registry::new(ctx(), vec![PoolSpec::Clmm(buy), PoolSpec::Clmm(clmm(11, BASE, T, 30, 31))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 16);
        assert_eq!(h[14].pubkey, vp::clmm_bitmap_extension_of(&vp::PANCAKE, &id(10)));
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_PANCAKE, direction: 1, n_accounts: 16, token_account_size: 0 });
    }

    #[test]
    fn byreal_stays_on_the_long_list() {
        let mut buy = clmm(10, BASE, T, 20, 21);
        buy.program = vp::BYREAL;
        buy.venue_v2 = VENUE_BYREAL;
        buy.venue_v1 = None;
        let mut src = MapSource::new();
        src.insert(id(10), hex(include_str!("testdata/clmm_ray.hex")));
        let reg = Registry::new(ctx(), vec![PoolSpec::Clmm(buy), PoolSpec::Clmm(clmm(11, BASE, T, 30, 31))]);
        assert!(matches!(reg.build_pools(&src)[0].kind, engine::Kind::Clmm { .. }));
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h[0].pubkey, vp::BYREAL);
        assert_eq!(h[9].pubkey, TOKEN);
        assert_eq!(h[10].pubkey, TOKEN_2022);
        assert_eq!(h[11].pubkey, MEMO);
        assert_eq!(h[14].pubkey, vp::clmm_bitmap_extension_of(&vp::BYREAL, &id(10)));
        assert_eq!(planned.plan_hops[0].venue, VENUE_BYREAL);
        assert_eq!(planned.plan_hops[0].direction, 1);
        assert_eq!(h.len(), 16);
    }

    fn mint_supply(supply: u64) -> Vec<u8> {
        let mut a = vec![0u8; 82];
        a[36..44].copy_from_slice(&supply.to_le_bytes());
        a
    }

    fn damm_v1_spec(pool: u8) -> DammV1PoolSpec {
        DammV1PoolSpec {
            pool: id(pool),
            mint_a: id(BASE),
            mint_b: id(T),
            a_vault: id(pool + 1),
            b_vault: id(pool + 2),
            a_token_vault: id(pool + 10),
            b_token_vault: id(pool + 11),
            a_lp_mint: id(pool + 12),
            b_lp_mint: id(pool + 13),
            a_vault_lp: id(pool + 14),
            b_vault_lp: id(pool + 15),
            protocol_fee_a: id(pool + 16),
            protocol_fee_b: id(pool + 17),
            fee_bps: 0,
        }
    }

    #[test]
    fn damm_v1_quotes_the_lp_share_and_swaps_the_fee_account() {
        let mut src = MapSource::new();
        src.insert(id(20), token_account(1_000_000));
        src.insert(id(21), token_account(4_000_000));
        src.insert(id(22), mint_supply(1_000_000));
        src.insert(id(23), mint_supply(1_000_000));
        src.insert(id(24), token_account(250_000));
        src.insert(id(25), token_account(500_000));
        let spec = damm_v1_spec(10);
        let reg = Registry::new(ctx(), vec![PoolSpec::DammV1(spec), PoolSpec::DammV1(damm_v1_spec(40))]);
        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 1);
        match built[0].kind {
            engine::Kind::Cp { res_a, res_b, .. } => {
                assert_eq!(res_a, 250_000);
                assert_eq!(res_b, 2_000_000);
            }
            _ => panic!("DAMM v1"),
        }
        src.insert(id(50), token_account(2_000_000));
        src.insert(id(51), token_account(1_000_000));
        src.insert(id(52), mint_supply(1_000_000));
        src.insert(id(53), mint_supply(1_000_000));
        src.insert(id(54), token_account(1_000_000));
        src.insert(id(55), token_account(1_000_000));
        let reg = Registry::new(ctx(), vec![PoolSpec::DammV1(damm_v1_spec(10)), PoolSpec::DammV1(damm_v1_spec(40))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(40), amount_in: 1_000, profit: 1 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 16);
        assert_eq!(h[0].pubkey, damm_v1::PROGRAM);
        assert_eq!(h[2].pubkey, id(200));
        assert_eq!(h[3].pubkey, id(201));
        assert_eq!(h[12].pubkey, id(26), "fee of side A");
        assert!(h[13].is_signer && !h[13].is_writable);
        assert_eq!(h[14].pubkey, damm_v1::VAULT_PROGRAM);
        assert_eq!(h[15].pubkey, TOKEN);
        assert_eq!(planned.plan_hops[0].venue, VENUE_DAMM_V1);
        assert_eq!(planned.plan_hops[0].direction, 0);
        assert_eq!(planned.hops[1][12].pubkey, id(57), "selling T takes the side B fee");
    }

    #[test]
    fn whirlpool_decode_and_hop() {
        let mut src = MapSource::new();
        src.insert(id(12), hex(include_str!("testdata/whirlpool.hex")));
        let reg = Registry::new(
            ctx(),
            vec![PoolSpec::Whirlpool(whirlpool(12, BASE, T)), PoolSpec::Whirlpool(whirlpool(13, BASE, T))],
        );
        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 1);
        assert!(matches!(built[0].kind, engine::Kind::Clmm { .. }), "Whirlpool → CLMM model");

        let opp = Opportunity { token: id(T), buy_pool: id(12), sell_pool: id(13), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0]; // base→T, base=mint_a → a_to_b (direction byte 1), SPL → swap
        assert_eq!(h.len(), 12);
        assert_eq!(h[0].pubkey, whirlpool::PROGRAM);
        assert_eq!(h[1].pubkey, TOKEN);
        assert!(h[2].is_signer, "token_authority = bot");
        let down = vp::whirlpool_tick_array_starts(100, 8, false, 3);
        assert_eq!(h[8].pubkey, vp::whirlpool_tick_array(&id(12), down[0]), "a→b: first array downward");
        assert_eq!(h[9].pubkey, vp::whirlpool_tick_array(&id(12), down[1]));
        assert_eq!(h[10].pubkey, vp::whirlpool_tick_array(&id(12), down[2]));
        assert_eq!(h[11].pubkey, vp::whirlpool_oracle(&id(12)), "oracle computed");
        assert_eq!(planned.plan_hops[0].venue, VENUE_WHIRLPOOL_V1);
        assert_eq!(planned.plan_hops[0].direction, 1);
    }

    #[test]
    fn whirlpool_token2022_keeps_swap_v2() {
        let mut spec = whirlpool(12, BASE, T);
        spec.token_program_b = TOKEN_2022;
        let reg = Registry::new(ctx(), vec![PoolSpec::Whirlpool(spec), PoolSpec::Whirlpool(whirlpool(13, BASE, T))]);
        let opp = Opportunity { token: id(T), buy_pool: id(12), sell_pool: id(13), amount_in: 100_000, profit: 80_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0];
        assert_eq!(h.len(), 18);
        assert_eq!(h[3].pubkey, MEMO);
        assert!(h[4].is_signer);
        let (core, supp) = vp::whirlpool_v2_window(100, 8);
        assert_eq!(h[12].pubkey, vp::whirlpool_tick_array(&id(12), core[0]));
        assert_eq!(h[13].pubkey, vp::whirlpool_tick_array(&id(12), core[1]));
        assert_eq!(h[14].pubkey, vp::whirlpool_tick_array(&id(12), core[2]));
        assert_eq!(h[16].pubkey, vp::whirlpool_tick_array(&id(12), supp[0]));
        assert_eq!(h[17].pubkey, vp::whirlpool_tick_array(&id(12), supp[1]));
        assert_eq!(planned.plan_hops[0].venue, VENUE_WHIRLPOOL);
        assert_eq!(planned.plan_hops[0].direction, 1);
        assert_eq!(planned.plan_hops[1].venue, VENUE_WHIRLPOOL_V1);
        assert_eq!(planned.hops[1].len(), 12);
    }

    #[test]
    fn plan_three_venue_mixed_route() {
        // Triangle across THREE different exchanges in one tx: CPMM → CLMM → Whirlpool.
        let ctx3 = BotCtx {
            bot: id(100),
            base_mint: id(BASE),
            token_accounts: vec![(id(BASE), id(200)), (id(T), id(201)), (id(U), id(202))],
        };
        let reg = Registry::new(
            ctx3,
            vec![
                PoolSpec::Cpmm(cpmm(10, 20, 21)),      // base→T
                PoolSpec::Clmm(clmm(11, T, U, 30, 31)), // T→U
                PoolSpec::Whirlpool(whirlpool(12, U, BASE)), // U→base
            ],
        );
        let opp = TriOpportunity {
            tokens: [id(T), id(U)],
            pools: [id(10), id(11), id(12)],
            amount_in: 100_000,
            profit: 120_000,
        };
        let planned = reg.plan_three_hop(&opp).expect("3-venue route");
        assert_eq!(planned.hops.len(), 3);
        assert_eq!(planned.intermediates.len(), 2);

        // Each leg from a different venue, each starts with the ID of its own program.
        assert_eq!(planned.plan_hops[0].venue, VENUE_CPMM);
        assert_eq!(planned.plan_hops[1].venue, VENUE_CLMM_V1);
        assert_eq!(planned.plan_hops[1].direction, 1, "a→b on 0x24 writes sqrt MIN+1");
        assert_eq!(planned.plan_hops[2].venue, VENUE_WHIRLPOOL_V1);
        assert_eq!(planned.plan_hops[2].direction, 1);
        assert_eq!(planned.hops[0][0].pubkey, cpmm::PROGRAM);
        assert_eq!(planned.hops[1][0].pubkey, clmm::PROGRAM);
        assert_eq!(planned.hops[2][0].pubkey, whirlpool::PROGRAM);
        assert_eq!((planned.hops[0].len(), planned.hops[1].len(), planned.hops[2].len()), (15, 12, 12));

        // Intermediate accounts: T (from CPMM) and U (from CLMM).
        assert_eq!(planned.intermediates[0][1].pubkey, id(T));
        assert_eq!(planned.intermediates[1][1].pubkey, id(U));
    }

    fn ame(n: u8) -> AccountMeta {
        AccountMeta { pubkey: id(n), is_signer: false, is_writable: false }
    }

    fn damm_v2(pool: u8, va: u8, vb: u8) -> DammV2PoolSpec {
        DammV2PoolSpec {
            pool: id(pool),
            mint_a: id(BASE),
            mint_b: id(T),
            vault_a: id(va),
            vault_b: id(vb),
            token_program_a: TOKEN,
            token_program_b: TOKEN,
            referral: None,
            fee_bps: 25,
        }
    }

    /// Pump pool: base = memecoin (T), quote = WSOL (base). Accounts resolved (snapshot).
    fn pump(pool: u8, basev: u8, quotev: u8) -> PumpPoolSpec {
        PumpPoolSpec {
            pool: id(pool),
            base_mint: id(T),
            quote_mint: id(BASE),
            base_vault: id(basev),
            quote_vault: id(quotev),
            base_token_program: TOKEN,
            quote_token_program: TOKEN,
            fee_recipient: id(pool + 40),
            fee_recipient_ata: id(pool + 41),
            creator_vault_ata: id(pool + 42),
            creator_vault_authority: id(pool + 43),
            extra: [ame(pool + 44), ame(pool + 45), ame(pool + 46), ame(pool + 47)],
            fee_bps: 25,
        }
    }

    #[test]
    fn damm_v2_decode_and_hop() {
        let mut src = MapSource::new();
        src.insert(id(20), token_account(1_000_000));
        src.insert(id(21), token_account(2_000_000));
        src.insert(id(30), token_account(2_000_000));
        src.insert(id(31), token_account(1_000_000));
        let reg = Registry::new(ctx(), vec![PoolSpec::DammV2(damm_v2(10, 20, 21)), PoolSpec::DammV2(damm_v2(11, 30, 31))]);

        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 2);
        assert!(matches!(built[0].kind, engine::Kind::Cp { .. }), "DAMM v2 → constant-product");

        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 50_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0]; // base→T, base=mint_a → dir 0
        assert_eq!(h.len(), 15);
        assert_eq!(h[0].pubkey, damm_v2::PROGRAM);
        assert_eq!(h[1].pubkey, damm_v2::POOL_AUTHORITY);
        assert_eq!(h[3].pubkey, id(200), "input = base ATA");
        assert_eq!(h[4].pubkey, id(201), "output = T account");
        assert!(h[9].is_signer, "payer = bot");
        assert_eq!(h[13].pubkey, damm_v2::EVENT_AUTHORITY);
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_DAMM_V2, direction: 0, n_accounts: 15, token_account_size: 0 });
    }

    #[test]
    fn pump_sell_leg_structure() {
        // Real layout: buy on CPMM (WSOL→T), sell on Pump (T→WSOL = base→quote = sell).
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Pump(pump(11, 30, 31))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 90_000 };
        let planned = reg.plan_two_hop(&opp).expect("CPMM→Pump");

        assert_eq!(planned.plan_hops[0].venue, VENUE_CPMM);
        assert_eq!(planned.plan_hops[1].venue, VENUE_PUMP);
        let h = &planned.hops[1]; // Pump sell
        assert_eq!(h.len(), 28, "Pump AMM = 28 accounts");
        assert_eq!(h[0].pubkey, pump_amm::PROGRAM);
        assert!(h[2].is_signer, "user = bot signs");
        assert_eq!(h[6].pubkey, id(201), "user_base_ata = T account (base = memecoin)");
        assert_eq!(h[7].pubkey, id(200), "user_quote_ata = WSOL ATA");
        // user_volume_accumulator COMPUTED from the bot key.
        assert_eq!(h[21].pubkey, vp::pump_user_volume_accumulator(&id(100)));
        assert_eq!(planned.plan_hops[1].direction, 0, "sell = direction 0");
    }

    #[test]
    fn pump_buy_leg_is_exact_quote_in() {
        // Pump as the BUY leg: WSOL→T = quote→base = buy_exact_quote_in, direction 1.
        let reg = Registry::new(ctx(), vec![PoolSpec::Pump(pump(10, 20, 21)), PoolSpec::Cpmm(cpmm(11, 30, 31))]);
        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 90_000 };
        let planned = reg.plan_two_hop(&opp).expect("Pump buy");
        assert_eq!(planned.plan_hops[0].venue, VENUE_PUMP);
        assert_eq!(planned.plan_hops[0].direction, 1);
        assert_eq!(planned.hops[0].len(), 28);
    }

    #[test]
    fn ray_v4_quotes_vaults_and_orders_source_then_destination() {
        let mut src = MapSource::new();
        src.insert(id(20), token_account(1_000_000));
        src.insert(id(21), token_account(2_000_000));
        src.insert(id(30), token_account(2_000_000));
        src.insert(id(31), token_account(1_000_000));
        let buy = RayV4PoolSpec {
            amm: id(10),
            coin_mint: id(BASE),
            pc_mint: id(T),
            coin_vault: id(20),
            pc_vault: id(21),
            fee_bps: 25,
        };
        let sell = RayV4PoolSpec {
            amm: id(11),
            coin_mint: id(BASE),
            pc_mint: id(T),
            coin_vault: id(30),
            pc_vault: id(31),
            fee_bps: 25,
        };
        let reg = Registry::new(ctx(), vec![PoolSpec::RayV4(buy), PoolSpec::RayV4(sell)]);
        let built = reg.build_pools(&src);
        assert_eq!(built.len(), 2);
        assert!(matches!(built[0].kind, engine::Kind::Cp { res_a: 1_000_000, res_b: 2_000_000, .. }));

        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 100_000, profit: 50_000 };
        let planned = reg.plan_two_hop(&opp).unwrap();
        let h = &planned.hops[0]; // base→T = coin→pc
        assert_eq!(h.len(), 9);
        assert_eq!(h[0].pubkey, ray_v4::PROGRAM);
        assert_eq!(h[1].pubkey, TOKEN);
        assert_eq!(h[3].pubkey, ray_v4::AUTHORITY);
        assert_eq!(h[6].pubkey, id(200), "source = base ATA");
        assert_eq!(h[7].pubkey, id(201), "destination = T account");
        assert!(h[8].is_signer);
        assert_eq!(planned.plan_hops[0], PlanHop { venue: VENUE_RAY_V4, direction: 0, n_accounts: 9, token_account_size: 0 });
        assert_eq!(planned.hops[1][6].pubkey, id(201), "sell: source = T account");
        assert_eq!(planned.hops[1][7].pubkey, id(200));
        assert_eq!(planned.plan_hops[1].direction, 1);
    }

    fn manifest_market(base_vault: u8, quote_vault: u8) -> Vec<u8> {
        const HEADER: usize = 256;
        const D18: u128 = 1_000_000_000_000_000_000;
        let disc: u64 = 4_859_840_929_024_028_656;
        let mut buf = vec![0u8; HEADER + 160];
        buf[0..8].copy_from_slice(&disc.to_le_bytes());
        buf[16..48].copy_from_slice(&id(T));
        buf[48..80].copy_from_slice(&id(BASE));
        buf[80..112].copy_from_slice(&id(base_vault));
        buf[112..144].copy_from_slice(&id(quote_vault));
        buf[156..160].copy_from_slice(&0u32.to_le_bytes());
        buf[160..164].copy_from_slice(&0u32.to_le_bytes());
        buf[164..168].copy_from_slice(&80u32.to_le_bytes());
        buf[168..172].copy_from_slice(&80u32.to_le_bytes());
        let put = |buf: &mut [u8], index: u32, price: u128, base: u64, is_bid: bool| {
            let i = HEADER + index as usize;
            buf[i..i + 12].fill(0xff);
            buf[i + 16..i + 32].copy_from_slice(&price.to_le_bytes());
            buf[i + 32..i + 40].copy_from_slice(&base.to_le_bytes());
            buf[i + 56] = u8::from(is_bid);
        };
        put(&mut buf, 0, 2 * D18, 1_000, true);
        put(&mut buf, 80, D18, 1_000, false);
        buf
    }

    #[test]
    fn manifest_quotes_the_book_and_sells_base_with_direction_1() {
        let spec = ManifestPoolSpec {
            market: id(11),
            base_mint: id(T),
            quote_mint: id(BASE),
            base_vault: id(70),
            quote_vault: id(71),
            token_program_base: TOKEN,
            token_program_quote: TOKEN,
        };
        let mut src = MapSource::new();
        src.insert(id(11), manifest_market(70, 71));
        src.insert(id(20), token_account(1_000_000));
        src.insert(id(21), token_account(1_000_000));
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Manifest(spec.clone())]);
        let built = reg.build_pools(&src);
        let book = built.iter().find(|p| p.id == id(11)).expect("book");
        assert!(matches!(book.kind, engine::Kind::Book { .. }));
        assert_eq!(book.quote(1, true), 2, "1 base at price 2 walks down into 2 quote");

        let opp = Opportunity { token: id(T), buy_pool: id(10), sell_pool: id(11), amount_in: 1_000, profit: 1 };
        let planned = reg.plan_two_hop(&opp).expect("CPMM→Manifest");
        assert_eq!(
            planned.plan_hops[1],
            PlanHop { venue: VENUE_MANIFEST, direction: 1, n_accounts: 13, token_account_size: 0 }
        );
        let h = &planned.hops[1];
        assert_eq!(h.len(), 13, "both SPL Token: no base mint and no second program");
        assert_eq!(h[0].pubkey, manifest::PROGRAM);
        assert_eq!(h[1].pubkey, id(100));
        assert_eq!(h[2].pubkey, id(100));
        assert_eq!(h[5].pubkey, id(201), "trader base = T account");
        assert_eq!(h[6].pubkey, id(200), "trader quote = base ATA");
        assert_eq!(h[9].pubkey, TOKEN);
        assert_eq!(h[10].pubkey, id(BASE), "quote mint stays");
        assert_eq!(h[11].pubkey, vp::manifest_global(&id(BASE)));
        assert_eq!(h[12].pubkey, vp::manifest_global_vault(&id(BASE)));

        let mut long = spec.clone();
        long.token_program_base = TOKEN_2022;
        let reg = Registry::new(ctx(), vec![PoolSpec::Cpmm(cpmm(10, 20, 21)), PoolSpec::Manifest(long)]);
        let planned = reg.plan_two_hop(&opp).expect("Token-2022 keeps 15 accounts");
        assert_eq!(planned.plan_hops[1].n_accounts, 15);
        assert_eq!(planned.hops[1][13].pubkey, vp::manifest_global(&id(BASE)));

        let mut bad = spec;
        bad.base_vault = id(72);
        let reg = Registry::new(ctx(), vec![PoolSpec::Manifest(bad)]);
        assert!(reg.build_pools(&src).is_empty(), "the spec's vault must be in the header");
    }
}
