//! Wiring the layers together: decoded pool + pool state + bot context → a ready route leg
//! ([`send::route`]). **Derived** accounts (oracle, bin_array, tick_array, observation, bitmap
//! extension, Manifest global, Pump volume accumulator) are computed by [`keys::venue_pda`] — they are not
//! copied in from outside. Pool fields (vaults, mints) come from the `pools` decoders, and the bot
//! accounts (user, base ATAs, intermediate accounts) and the variable-tail indexes are supplied by the route planner.
//!
//! This closes the construction: `feed::route::*_hop(...)` → [`send::route::assemble_route`] →
//! [`send::tx::build_arb_message`] = bytes to sign. Signing and sending: on the operator's side.

use keys::venue_pda as vp;
use send::route;
use send::tx::Pubkey;

/// Plan of a DLMM leg (fields from the LbPair decoder + the planner). `bitmap_extension`/`host_fee_in` = `None`
/// when absent (program-sentinel). `bin_array_indexes` from the pool's `active_id` (see
/// [`keys::venue_pda::dlmm_bin_array_index`]).
pub struct DlmmPlan {
    pub lb_pair: Pubkey,
    pub bitmap_extension: Option<Pubkey>,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub user_token_in: Pubkey,
    pub user_token_out: Pubkey,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub host_fee_in: Option<Pubkey>,
    pub user: Pubkey,
    pub token_x_program: Pubkey,
    pub token_y_program: Pubkey,
    pub bin_array_indexes: Vec<i64>,
}

pub fn dlmm_hop(plan: &DlmmPlan) -> route::dlmm::DlmmHop {
    route::dlmm::DlmmHop {
        lb_pair: plan.lb_pair,
        bitmap_extension: plan.bitmap_extension.unwrap_or(vp::DLMM),
        reserve_x: plan.reserve_x,
        reserve_y: plan.reserve_y,
        user_token_in: plan.user_token_in,
        user_token_out: plan.user_token_out,
        token_x_mint: plan.token_x_mint,
        token_y_mint: plan.token_y_mint,
        oracle: vp::dlmm_oracle(&plan.lb_pair),
        host_fee_in: plan.host_fee_in.unwrap_or(vp::DLMM),
        user: plan.user,
        token_x_program: plan.token_x_program,
        token_y_program: plan.token_y_program,
        bin_arrays: plan.bin_array_indexes.iter().map(|&i| vp::dlmm_bin_array(&plan.lb_pair, i)).collect(),
    }
}

/// Plan of a Raydium CLMM leg. Tail = `[bitmap_extension?, tick_array…]` (computed from `pool_state`).
pub struct ClmmPlan {
    pub program: Pubkey,
    pub payer: Pubkey,
    pub amm_config: Pubkey,
    pub pool_state: Pubkey,
    pub input_token_account: Pubkey,
    pub output_token_account: Pubkey,
    pub input_vault: Pubkey,
    pub output_vault: Pubkey,
    pub input_vault_mint: Pubkey,
    pub output_vault_mint: Pubkey,
    pub include_bitmap_extension: bool,
    /// Pancake v1 appends the bitmap at the end of the tail. The other CLMMs keep it at the start.
    pub bitmap_last: bool,
    pub tick_array_starts: Vec<i32>,
}

pub fn clmm_hop(plan: &ClmmPlan) -> route::clmm::ClmmHop {
    let mut ticks = Vec::new();
    for &s in &plan.tick_array_starts {
        ticks.push(vp::clmm_tick_array_of(&plan.program, &plan.pool_state, s));
    }
    let mut tail = Vec::new();
    let bitmap = vp::clmm_bitmap_extension_of(&plan.program, &plan.pool_state);
    if plan.include_bitmap_extension && !plan.bitmap_last {
        tail.push(bitmap);
    }
    tail.extend(ticks);
    if plan.include_bitmap_extension && plan.bitmap_last {
        tail.push(bitmap);
    }
    route::clmm::ClmmHop {
        program: plan.program,
        payer: plan.payer,
        amm_config: plan.amm_config,
        pool_state: plan.pool_state,
        input_token_account: plan.input_token_account,
        output_token_account: plan.output_token_account,
        input_vault: plan.input_vault,
        output_vault: plan.output_vault,
        observation_state: vp::clmm_observation_of(&plan.program, &plan.pool_state),
        input_vault_mint: plan.input_vault_mint,
        output_vault_mint: plan.output_vault_mint,
        tick_arrays: tail,
    }
}

/// BinArray accounts of a DLMM leg computed from pool state: from the array containing `active_id` in the swap direction.
/// The result feeds [`DlmmPlan::bin_array_indexes`] indirectly — here we return the account addresses directly.
pub fn dlmm_bin_array_accounts(lb_pair: &Pubkey, active_id: i32, ascending: bool, count: u32) -> Vec<Pubkey> {
    vp::dlmm_bin_array_indices(active_id, ascending, count)
        .into_iter()
        .map(|i| vp::dlmm_bin_array(lb_pair, i))
        .collect()
}

/// Variable tail of a CLMM leg from pool state: `[bitmap_extension?, tick_array…]`, arrays from
/// `current_tick` in the swap direction. Goes into [`ClmmHop::tick_arrays`].
pub fn clmm_tail_accounts(
    program: &Pubkey,
    pool_state: &Pubkey,
    current_tick: i32,
    tick_spacing: u16,
    ascending: bool,
    count: u32,
    include_bitmap: bool,
) -> Vec<Pubkey> {
    let mut v = Vec::new();
    if include_bitmap {
        v.push(vp::clmm_bitmap_extension_of(program, pool_state));
    }
    for s in vp::clmm_tick_array_starts(current_tick, tick_spacing, ascending, count) {
        v.push(vp::clmm_tick_array_of(program, pool_state, s));
    }
    v
}

/// Plan of a Raydium CPMM leg (observation computed from `pool_state`).
pub struct CpmmPlan {
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
}

pub fn cpmm_hop(plan: &CpmmPlan) -> route::cpmm::CpmmHop {
    route::cpmm::CpmmHop {
        payer: plan.payer,
        amm_config: plan.amm_config,
        pool_state: plan.pool_state,
        input_token_account: plan.input_token_account,
        output_token_account: plan.output_token_account,
        input_vault: plan.input_vault,
        output_vault: plan.output_vault,
        input_token_program: plan.input_token_program,
        output_token_program: plan.output_token_program,
        input_token_mint: plan.input_token_mint,
        output_token_mint: plan.output_token_mint,
        observation_state: vp::cpmm_observation(&plan.pool_state),
    }
}

/// Plan of an Orca Whirlpool leg. The oracle and tick arrays are computed by [`keys::venue_pda`] when the pool spec
/// has `tick_current` and `tick_spacing`. They stay explicit here, because this planner assembles already computed keys.
pub struct WhirlpoolPlan {
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
    pub supplemental_tick_arrays: Vec<Pubkey>,
}

pub fn whirlpool_hop(plan: &WhirlpoolPlan) -> route::whirlpool::WhirlpoolHop {
    route::whirlpool::WhirlpoolHop {
        token_program_a: plan.token_program_a,
        token_program_b: plan.token_program_b,
        token_authority: plan.token_authority,
        whirlpool: plan.whirlpool,
        token_mint_a: plan.token_mint_a,
        token_mint_b: plan.token_mint_b,
        token_owner_a: plan.token_owner_a,
        token_vault_a: plan.token_vault_a,
        token_owner_b: plan.token_owner_b,
        token_vault_b: plan.token_vault_b,
        tick_array_0: plan.tick_array_0,
        tick_array_1: plan.tick_array_1,
        tick_array_2: plan.tick_array_2,
        oracle: vp::whirlpool_oracle(&plan.whirlpool),
        supplemental_tick_arrays: plan.supplemental_tick_arrays.clone(),
    }
}

/// Plan of a Meteora DAMM v2 leg (no derived PDAs per pool; `referral` = `None` → program-sentinel).
pub struct DammV2Plan {
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
    pub referral: Option<Pubkey>,
}

pub fn damm_v2_hop(plan: &DammV2Plan) -> route::damm_v2::DammV2Hop {
    route::damm_v2::DammV2Hop {
        pool: plan.pool,
        input_token_account: plan.input_token_account,
        output_token_account: plan.output_token_account,
        token_a_vault: plan.token_a_vault,
        token_b_vault: plan.token_b_vault,
        token_a_mint: plan.token_a_mint,
        token_b_mint: plan.token_b_mint,
        payer: plan.payer,
        token_a_program: plan.token_a_program,
        token_b_program: plan.token_b_program,
        referral_token_account: plan.referral.unwrap_or(route::damm_v2::PROGRAM),
    }
}

/// Plan of a Manifest leg (`global`/`global_vault` computed from the quote mint).
pub struct ManifestPlan {
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
}

pub fn manifest_hop(plan: &ManifestPlan) -> route::manifest::ManifestHop {
    route::manifest::ManifestHop {
        payer: plan.payer,
        owner: plan.owner,
        market: plan.market,
        trader_base: plan.trader_base,
        trader_quote: plan.trader_quote,
        base_vault: plan.base_vault,
        quote_vault: plan.quote_vault,
        token_program_base: plan.token_program_base,
        base_mint: plan.base_mint,
        token_program_quote: plan.token_program_quote,
        quote_mint: plan.quote_mint,
        global: vp::manifest_global(&plan.quote_mint),
        global_vault: vp::manifest_global_vault(&plan.quote_mint),
    }
}

/// Plan of a Pump AMM leg (`user_volume_accumulator` computed from `user`; creator vaults and the fee ATA
/// are supplied by the planner). Mints/vaults from the Pump decoder, user/ATA from the bot context.
pub struct PumpPlan {
    pub pool: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub user: Pubkey,
    pub user_base_ata: Pubkey,
    pub user_quote_ata: Pubkey,
    pub fee_recipient: Pubkey,
    pub fee_recipient_ata: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    pub creator_vault_ata: Pubkey,
    pub creator_vault_authority: Pubkey,
    pub extra: [send::tx::AccountMeta; 4],
}

pub fn pump_hop(plan: &PumpPlan) -> route::pump_amm::PumpAmmHop {
    route::pump_amm::PumpAmmHop {
        pool: plan.pool,
        base_mint: plan.base_mint,
        quote_mint: plan.quote_mint,
        base_vault: plan.base_vault,
        quote_vault: plan.quote_vault,
        user: plan.user,
        user_base_ata: plan.user_base_ata,
        user_quote_ata: plan.user_quote_ata,
        fee_recipient: plan.fee_recipient,
        fee_recipient_ata: plan.fee_recipient_ata,
        base_token_program: plan.base_token_program,
        quote_token_program: plan.quote_token_program,
        creator_vault_ata: plan.creator_vault_ata,
        creator_vault_authority: plan.creator_vault_authority,
        user_volume_accumulator: vp::pump_user_volume_accumulator(&plan.user),
        extra: plan.extra.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use send::tx::AccountMeta;

    fn hexb(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    fn fixture(hexstr: &str) -> Vec<Pubkey> {
        let raw = hexb(hexstr.trim());
        (0..raw.len() / 32)
            .map(|i| {
                let mut k = [0u8; 32];
                k.copy_from_slice(&raw[i * 32..i * 32 + 32]);
                k
            })
            .collect()
    }
    fn ro(pk: Pubkey) -> AccountMeta {
        AccountMeta { pubkey: pk, is_signer: false, is_writable: false }
    }
    /// Derived accounts assembled by feed must reproduce the real slice key for key.
    fn assert_eq_slice(got: &[AccountMeta], want: &[Pubkey], tag: &str) {
        assert_eq!(got.len(), want.len(), "{tag}: account count");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g.pubkey, *w, "{tag}: position {i}");
        }
    }

    #[test]
    fn dlmm_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/dlmm_hop_accounts.hex"));
        let plan = DlmmPlan {
            lb_pair: w[1],
            bitmap_extension: if w[2] == vp::DLMM { None } else { Some(w[2]) },
            reserve_x: w[3],
            reserve_y: w[4],
            user_token_in: w[5],
            user_token_out: w[6],
            token_x_mint: w[7],
            token_y_mint: w[8],
            host_fee_in: if w[10] == vp::DLMM { None } else { Some(w[10]) },
            user: w[11],
            token_x_program: w[12],
            token_y_program: w[13],
            bin_array_indexes: vec![-11], // from the pool's active_id; here from a real tx
        };
        // oracle (pos.9) and bin_array (pos.17) COMPUTED by feed, not copied.
        assert_eq_slice(&dlmm_hop(&plan).accounts(), &w, "DLMM");
    }

    #[test]
    fn clmm_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/clmm_hop_accounts.hex"));
        let plan = ClmmPlan {
            program: vp::CLMM,
            payer: w[1],
            amm_config: w[2],
            pool_state: w[3],
            input_token_account: w[4],
            output_token_account: w[5],
            input_vault: w[6],
            output_vault: w[7],
            input_vault_mint: w[12],
            output_vault_mint: w[13],
            include_bitmap_extension: true, // pos.14 = bitmap extension
            bitmap_last: false,
            tick_array_starts: vec![-55200, -54600], // pos.15, 16
        };
        // observation (9), bitmap_extension (14), tick_array (15,16) COMPUTED by feed.
        assert_eq_slice(&clmm_hop(&plan).accounts(), &w, "CLMM");
    }

    #[test]
    fn cpmm_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/cpmm_hop_accounts.hex"));
        let plan = CpmmPlan {
            payer: w[1],
            amm_config: w[3],
            pool_state: w[4],
            input_token_account: w[5],
            output_token_account: w[6],
            input_vault: w[7],
            output_vault: w[8],
            input_token_program: w[9],
            output_token_program: w[10],
            input_token_mint: w[11],
            output_token_mint: w[12],
        };
        assert_eq_slice(&cpmm_hop(&plan).accounts(), &w, "CPMM"); // observation (13) computed
    }

    #[test]
    fn whirlpool_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/whirlpool_hop_accounts.hex"));
        let plan = WhirlpoolPlan {
            token_program_a: w[1],
            token_program_b: w[2],
            token_authority: w[4],
            whirlpool: w[5],
            token_mint_a: w[6],
            token_mint_b: w[7],
            token_owner_a: w[8],
            token_vault_a: w[9],
            token_owner_b: w[10],
            token_vault_b: w[11],
            tick_array_0: w[12],
            tick_array_1: w[13],
            tick_array_2: w[14],
            supplemental_tick_arrays: w[16..].to_vec(),
        };
        assert_eq_slice(&whirlpool_hop(&plan).accounts(), &w, "Whirlpool"); // oracle (15) computed
    }

    #[test]
    fn damm_v2_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/damm_v2_hop_accounts.hex"));
        let plan = DammV2Plan {
            pool: w[2],
            input_token_account: w[3],
            output_token_account: w[4],
            token_a_vault: w[5],
            token_b_vault: w[6],
            token_a_mint: w[7],
            token_b_mint: w[8],
            payer: w[9],
            token_a_program: w[10],
            token_b_program: w[11],
            referral: if w[12] == route::damm_v2::PROGRAM { None } else { Some(w[12]) },
        };
        assert_eq_slice(&damm_v2_hop(&plan).accounts(), &w, "DAMM v2");
    }

    #[test]
    fn manifest_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/manifest_hop_accounts.hex"));
        let plan = ManifestPlan {
            payer: w[1],
            owner: w[2],
            market: w[3],
            trader_base: w[5],
            trader_quote: w[6],
            base_vault: w[7],
            quote_vault: w[8],
            token_program_base: w[9],
            base_mint: w[10],
            token_program_quote: w[11],
            quote_mint: w[12],
        };
        // global (13), global_vault (14) COMPUTED by feed from the quote mint.
        assert_eq_slice(&manifest_hop(&plan).accounts(), &w, "Manifest");
    }

    fn pk(n: u8) -> Pubkey {
        let mut k = [0u8; 32];
        k[0] = n;
        k[31] = n;
        k
    }

    #[test]
    fn variable_tails_from_pool_state_match_real() {
        // DLMM: active_id -750 (→ index -11), 1 array → BinArray address from the real tx (pos.17).
        let d = fixture(include_str!("../../send/src/testdata/dlmm_hop_accounts.hex"));
        let ba = dlmm_bin_array_accounts(&d[1], -750, true, 1);
        assert_eq!(ba, vec![d[17]], "BinArray computed from pool state = real");

        // CLMM: current_tick -55000, upward, 2 arrays + bitmap → [bitmap(14), tick(15), tick(16)].
        let c = fixture(include_str!("../../send/src/testdata/clmm_hop_accounts.hex"));
        let tail = clmm_tail_accounts(&vp::CLMM, &c[3], -55000, 10, true, 2, true);
        assert_eq!(tail, vec![c[14], c[15], c[16]], "CLMM tail computed from pool state = real");
    }

    #[test]
    fn full_construction_path_to_signable_bytes() {
        // Closing the loop: feed::route (DLMM leg + Pump leg with computed derived accounts)
        // → assemble_route (prefix + intermediate triple + legs, invariant Σhop+3n+4)
        // → encode_route (instruction data) → build_arb_message (v0 message, here without ALT tables).
        let d = fixture(include_str!("../../send/src/testdata/dlmm_hop_accounts.hex"));
        let dlmm = dlmm_hop(&DlmmPlan {
            lb_pair: d[1],
            bitmap_extension: if d[2] == vp::DLMM { None } else { Some(d[2]) },
            reserve_x: d[3],
            reserve_y: d[4],
            user_token_in: d[5],
            user_token_out: d[6],
            token_x_mint: d[7],
            token_y_mint: d[8],
            host_fee_in: if d[10] == vp::DLMM { None } else { Some(d[10]) },
            user: d[11],
            token_x_program: d[12],
            token_y_program: d[13],
            bin_array_indexes: vec![-11],
        })
        .accounts();

        let pf = fixture(include_str!("../../send/src/testdata/pump_hop_accounts.hex"));
        let pump = pump_hop(&PumpPlan {
            pool: pf[1],
            base_mint: pf[4],
            quote_mint: pf[5],
            base_vault: pf[8],
            quote_vault: pf[9],
            user: pf[2],
            user_base_ata: pf[6],
            user_quote_ata: pf[7],
            fee_recipient: pf[10],
            fee_recipient_ata: pf[11],
            base_token_program: pf[12],
            quote_token_program: pf[13],
            creator_vault_ata: pf[18],
            creator_vault_authority: pf[19],
            extra: [ro(pf[24]), ro(pf[25]), ro(pf[26]), ro(pf[27])],
        })
        .accounts();

        let (n_dlmm, n_pump) = (dlmm.len() as u8, pump.len() as u8);
        // Bot = the only signer: fee_payer == user/authority in every leg (same key),
        // so all signer positions merge into a single signature. d[11] = pf[2] = bot.
        let bot = d[11];
        assert_eq!(bot, pf[2], "both legs are signed by the same bot");
        let prefix = route::fixed_prefix(bot, pk(2), pk(3), pk(4), pk(5), pk(6));
        let inter = [route::intermediate_triple(pk(7), pk(8), pk(9))];
        let route_accounts =
            route::assemble_route(&prefix, &inter, &[dlmm, pump], &[n_dlmm, n_pump]).expect("assemble");
        // Invariant: 7 + 3 + n_dlmm + n_pump.
        assert_eq!(route_accounts.len(), 7 + 3 + n_dlmm as usize + n_pump as usize);

        // Route data consistent with the declared number of accounts per leg.
        let route_data = engine::encode::encode_route(
            1_000_000,
            5_000,
            &[
                engine::encode::PlanHop { venue: 0x01, direction: 0, n_accounts: n_dlmm, token_account_size: 0 },
                engine::encode::PlanHop { venue: 0x05, direction: 0, n_accounts: n_pump, token_account_size: 0 },
            ],
            &engine::encode::RouteMeta::new(1, 0),
        )
        .expect("2-leg route");

        let msg = send::tx::build_arb_message(
            bot,         // fee_payer = bot (only signature)
            pk(200),     // recent blockhash
            pk(100),     // arbitrage program
            route_data,
            route_accounts,
            127_000,     // cu_limit
            131_072,     // loaded-accounts-data-size
            61_825,      // cu_price
            None,        // no tip
            &[],
        );
        assert_eq!(msg[0], 0x80, "v0 message");
        assert_eq!(msg[1], 1, "one signature (fee_payer)");
        assert!(msg.len() > 200, "non-empty message to sign");
    }

    #[test]
    fn pump_feed_reproduces_real_slice() {
        let w = fixture(include_str!("../../send/src/testdata/pump_hop_accounts.hex"));
        let plan = PumpPlan {
            pool: w[1],
            base_mint: w[4],
            quote_mint: w[5],
            base_vault: w[8],
            quote_vault: w[9],
            user: w[2],
            user_base_ata: w[6],
            user_quote_ata: w[7],
            fee_recipient: w[10],
            fee_recipient_ata: w[11],
            base_token_program: w[12],
            quote_token_program: w[13],
            creator_vault_ata: w[18],
            creator_vault_authority: w[19],
            extra: [ro(w[24]), ro(w[25]), ro(w[26]), ro(w[27])],
        };
        // user_volume_accumulator (21) COMPUTED by feed from the user key.
        assert_eq_slice(&pump_hop(&plan).accounts(), &w, "Pump");
    }
}
