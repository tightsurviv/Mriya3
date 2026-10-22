//! Runner loop skeleton: **pool state read → opportunity detection → package to sign**.
//!
//! Ties the layers into one pass (`tick`): takes the best opportunity from the loaded [`engine::Engine`]
//! (2-, 3-, 4- or 5-leg), builds the route (prefix + legs from [`RoutePlanner`]), encodes the instruction
//! data, computes the economics (fee + tip) and builds the **message bytes to sign**
//! ([`send::package::SignablePackage`]) — in fresh blockhash or durable nonce mode.
//!
//! The runner **does not sign or send**. Two explicit operator/integration seams:
//! - [`RoutePlanner`] — turns an opportunity into concrete DEX leg accounts (in production:
//!   [`crate::route`] with computed derived accounts and variable tails from pool state);
//! - [`Submitter`] — takes the finished package, signs and sends it (TPU/SWQoS or auction). The default
//!   [`DryRunSubmitter`] only collects packages and sends NOTHING (full loop testable without a network).
//!
//! Live data fetching ([`poll_once`]) goes through [`crate::transport::CurlSource`] and is a
//! thin wrapper around the tested core ([`build_engine`] + [`Runner::tick`]).

use crate::transport::CurlSource;
use crate::AccountSource;
use engine::encode::{encode_route, PlanHop, RouteMeta};
use engine::{CycleOpportunity, Engine, Opportunity, Pool, TriOpportunity, VictimSwap};
use send::package::{Blockhash, Channel, SignablePackage};
use send::route;
use send::tx::{AccountMeta, Pubkey};
use send::{bid, fee, priority};

/// Runner parameters (constant bot accounts, program, CU budget, tip policy, nonce mode).
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// Base token of the cycle (usually WSOL).
    pub base: Pubkey,
    /// **Public** bot key — fee_payer and authority (the only signature). No private key.
    pub bot: Pubkey,
    /// ID of our on-chain program.
    pub arb_program: Pubkey,
    /// Fixed route prefix.
    pub state_pda: Pubkey,
    pub base_ata: Pubkey,
    pub native_bridge_pda: Pubkey,
    pub wsol_mint: Pubkey,
    pub rent_sysvar: Pubkey,
    /// Upper input limit (available base capital).
    pub max_input: u64,
    /// Minimum gross profit to consider an opportunity (engine filter), in atoms of the base token.
    pub min_profit: u64,
    /// SOL price, 6 decimal places. Supplied by the caller; we do not invent a rate.
    /// A base equal to [`Self::wsol_mint`] writes 0 and leaves the profit in lamports.
    /// Any other base needs a price: the program and bid compute `profit · 1e9 / price`.
    pub sol_price: u64,
    /// Minimum net margin required to build a package at all.
    pub min_margin: u64,
    /// CU budget and price.
    pub cu_limit: u32,
    pub cu_price_micro: u64,
    pub loaded_accounts_data_bytes: u32,
    /// Auction tip fraction (bps). 0 → public channel (no tip).
    /// 3410 is the measurement `bid = round(0.341 · E)`.
    pub auction_frac_bps: u32,
    /// Below this predicted gross the auction does not build a package.
    /// The measured threshold is [`send::bid::MIN_AUCTION_E`].
    pub auction_min_e: u64,
    /// Auction shape. `2` and `4` = tip and CU price 0; the tip account is supplied by the caller
    /// (lane 4 uses Helius Sender accounts, which are not hardcoded here).
    /// `5` = Harmonic: bid as priority fee, always a fresh blockhash, no tip.
    /// `6` = BAM: bid as priority fee, nonce per configuration, no tip.
    /// Any other value stays with the tip shape and byte 2. Does not build a bundle.
    pub auction_lane: u8,
    /// Recipient account of the auction tip (Jito/BAM/…); required for the auction channel.
    pub tip_account: Option<Pubkey>,
    /// Whether to add `CreateIdempotent` for the intermediate accounts (point A) before the program in the same tx.
    /// `true` = safe default (idempotent); `false` when the operator creates them separately.
    pub create_intermediate_accounts: bool,
    /// Durable nonce mode.
    pub use_durable_nonce: bool,
    /// Nonce account, when [`Self::nonce_pair`] is `None`.
    pub nonce_account: Pubkey,
    pub nonce_authority: Pubkey,
    /// ALT tables supplied by the caller. An empty list gives a v0 message without lookups.
    /// Lookup #0 is the first table in this list that has a hit. The contents of Mriya's
    /// table are not here: we do not guess indexes.
    pub alts: Vec<send::tx::AddressLookupTable>,
    /// Pair from the `execution-n-NNNN` pool, derived from [`Self::bot`]. `Some(k)` takes the
    /// even account of pair `k`. The odd partner does not enter the transaction. `None` leaves
    /// [`Self::nonce_account`]. The pool counter does not advance on its own.
    pub nonce_pair: Option<u16>,
    /// COPY byte in the route trailer. Supplied by the caller. Zero leaves the field as before.
    /// Copy fan-out 0..5 does not enable itself: the measurement gives no selector.
    pub copy_index: u8,
}

/// Assembled route ready for encoding: intermediate accounts (triples) + legs (accounts) + leg metadata.
/// Invariant: `plan_hops[i].n_accounts == hops[i].len()` and `intermediates.len()+1 == hops.len()`.
pub struct PlannedRoute {
    pub intermediates: Vec<[AccountMeta; 3]>,
    pub hops: Vec<Vec<AccountMeta>>,
    pub plan_hops: Vec<PlanHop>,
}

/// Route planner seam: turns a detected opportunity into concrete per-venue leg accounts.
///
/// In production the implementation uses [`crate::route`] (`dlmm_hop`/`cpmm_hop`/…) with computed
/// derived accounts (oracle, tick_array, observation) and variable tails from pool state. Without this
/// the runner does not know the DEX account layout — hence an explicit integration point, not baked-in logic.
pub trait RoutePlanner {
    fn plan_two_hop(&self, opp: &Opportunity) -> Option<PlannedRoute>;
    fn plan_three_hop(&self, opp: &TriOpportunity) -> Option<PlannedRoute>;
    /// 4- or 5-leg cycle. None by default: the shorter route stays when the planner does not build it.
    fn plan_cycle(&self, _opp: &CycleOpportunity) -> Option<PlannedRoute> {
        None
    }
}

/// Data input seam: which accounts to fetch and how to assemble engine pools from them.
pub trait PoolBuilder {
    /// Account keys to fetch in this cycle (pools + vaults + states + tables).
    fn account_keys(&self) -> Vec<Pubkey>;
    /// Assemble engine pools from the fetched source (decoders from [`crate`]).
    fn build_pools(&self, src: &dyn AccountSource) -> Vec<Pool>;
}

/// Operator seam: takes the finished package, signs it with the bot key and sends. In production — TPU/SWQoS
/// or auction, with consent. Interface only here; the implementation with key and network is outside this crate.
pub trait Submitter {
    fn submit(&mut self, pkg: SignablePackage);
}

/// Default "dry run" receiver: collects packages, signs and sends NOTHING. Lets the whole loop
/// (feed → opportunity → package) run without a network or a key — for tests and preview.
#[derive(Default)]
pub struct DryRunSubmitter {
    pub packages: Vec<SignablePackage>,
}

impl Submitter for DryRunSubmitter {
    fn submit(&mut self, pkg: SignablePackage) {
        self.packages.push(pkg);
    }
}

/// Builds the engine from pools assembled by `builder` from the account source. Pure — testable with `MapSource`.
pub fn build_engine(cfg: &RunnerConfig, builder: &dyn PoolBuilder, src: &dyn AccountSource) -> Engine {
    let mut e = Engine::new(cfg.base, cfg.max_input, cfg.min_profit);
    for p in builder.build_pools(src) {
        e.add_pool(p);
    }
    e
}

/// Runner: `cfg` + route planner. The logic core (`tick`) is pure and testable.
/// Where we take the CU limit and price from.
enum Budget {
    /// Price and limit from config, auction tip, threshold B = fee + tip.
    Auction,
    /// Scheduler priority equal to `target_rp`, no tip, threshold B = 0.
    Backrun { target_rp: u64 },
}

pub struct Runner<'a> {
    pub cfg: &'a RunnerConfig,
    pub planner: &'a dyn RoutePlanner,
}

/// Result of one `PacketBatchUpdate` on the backrun path.
pub enum RelayerTick {
    Heartbeat { count: u64 },
    Packages { expiry_ms: u32, packages: Vec<SignablePackage> },
}

impl<'a> Runner<'a> {
    /// One pass on a ready engine. `blockhash_or_nonce` is a fresh blockhash (normal mode)
    /// or a durable nonce value from [`keys::durable_nonce`] (nonce mode — per `cfg.use_durable_nonce`).
    /// Returns the package to sign for the best **profitable** opportunity, or `None`.
    pub fn tick(&self, engine: &Engine, blockhash_or_nonce: Pubkey) -> Option<SignablePackage> {
        let (gross, amount_in, planned) = self.choose_route(engine)?;
        self.build_package(engine.base, gross, amount_in, planned, blockhash_or_nonce, Budget::Auction)
    }

    /// Backrun after the user's pending transaction.
    ///
    /// The Agave scheduler priority is 1 lower than the victim's, so the conflict queue executes
    /// its swap first. The package contains neither its transaction nor a leg before it. The auction tip
    /// is disabled and threshold B in the program is 0 (a positive profit in lamports is enough).
    /// `None` when the victim has no CU limit, its priority is 0, or the profit in lamports is 0.
    pub fn tick_backrun(
        &self,
        engine: &Engine,
        blockhash_or_nonce: Pubkey,
        victim: &crate::pending::DecodedTx,
    ) -> Option<SignablePackage> {
        let target_rp = crate::pending::scheduler_priority(victim)?.checked_sub(1)?;
        let (gross, amount_in, planned) = self.choose_route(engine)?;
        self.build_package(engine.base, gross, amount_in, planned, blockhash_or_nonce, Budget::Backrun { target_rp })
    }

    /// Highest gross profit. On a tie the shorter route wins (2 before 3, 4 before 5).
    /// A 4- or 5-leg cycle enters only when its profit is strictly higher
    /// and the planner can build the accounts. Otherwise the 2- or 3-leg route stays.
    fn choose_route(&self, engine: &Engine) -> Option<(u64, u64, PlannedRoute)> {
        let best2 = engine.find_two_hop().into_iter().next();
        let best3 = engine.find_three_hop().into_iter().next();
        let gross2 = best2.as_ref().map(|o| o.profit).unwrap_or(0);
        let gross3 = best3.as_ref().map(|o| o.profit).unwrap_or(0);
        let mut choice = if gross2 == 0 && gross3 == 0 {
            None
        } else if gross3 > gross2 {
            let o = best3?;
            Some((o.profit, o.amount_in, self.planner.plan_three_hop(&o)?))
        } else {
            let o = best2?;
            Some((o.profit, o.amount_in, self.planner.plan_two_hop(&o)?))
        };
        let mut gross = choice.as_ref().map(|c| c.0).unwrap_or(0);
        for hops in [4usize, 5] {
            let Some(op) = engine.find_cycle(hops).into_iter().next() else { continue };
            if op.profit > gross {
                if let Some(planned) = self.planner.plan_cycle(&op) {
                    gross = op.profit;
                    choice = Some((op.profit, op.amount_in, planned));
                }
            }
        }
        choice
    }

    /// Backrun computed on the state **after** the user's swap. Its transaction is not in the package.
    /// The original `engine` stays as it was before this swap.
    pub fn tick_backrun_after(
        &self,
        engine: &Engine,
        blockhash_or_nonce: Pubkey,
        victim: &crate::pending::DecodedTx,
        swap: VictimSwap,
    ) -> Option<SignablePackage> {
        let post = engine.after_victim(swap)?;
        self.tick_backrun(&post, blockhash_or_nonce, victim)
    }

    /// Backrun read from the pending transaction: amount, pool and direction from its instruction,
    /// scheduler priority 1 below its transaction.
    pub fn tick_backrun_tx(
        &self,
        engine: &Engine,
        blockhash_or_nonce: Pubkey,
        tx: &crate::pending::DecodedTx,
    ) -> Option<SignablePackage> {
        let swaps = crate::pending::victim_swaps(tx, &engine.pools);
        if swaps.is_empty() {
            return None;
        }
        let post = engine.after_victims(&swaps)?;
        self.tick_backrun(&post, blockhash_or_nonce, tx)
    }

    /// Backrun from a single relayer packet. The bytes are already truncated to `meta.size`.
    ///
    /// A vote and a packet marked as discarded yield no package. The rest is decoded
    /// (legacy, v0 or v1) and goes through [`Self::tick_backrun_tx`].
    pub fn tick_backrun_packet(
        &self,
        engine: &Engine,
        blockhash_or_nonce: Pubkey,
        packet: &crate::pending::Packet<'_>,
    ) -> Option<SignablePackage> {
        if packet.discard || packet.simple_vote {
            return None;
        }
        let tx = crate::pending::decode_transaction(packet.bytes)?;
        self.tick_backrun_tx(engine, blockhash_or_nonce, &tx)
    }

    /// Backrun from a single `PacketBatchUpdate`: the bodies that the private orderflow relayer pushes
    /// in `BlockEngineRelayer.StartExpiringPacketStream`.
    ///
    /// Each packet goes separately through [`Self::tick_backrun_packet`], from the state
    /// passed in `engine`. A vote, a discard and `meta.size` larger than the buffer
    /// yield no package. A heartbeat builds no route.
    /// The package stays in the race (victim priority − 1, tip 0, threshold B = 0).
    pub fn tick_backrun_update(
        &self,
        engine: &Engine,
        blockhash_or_nonce: Pubkey,
        update: &[u8],
    ) -> Option<RelayerTick> {
        match crate::relayer::decode_packet_batch_update(update)? {
            crate::relayer::RelayerUpdate::Heartbeat { count } => Some(RelayerTick::Heartbeat { count }),
            crate::relayer::RelayerUpdate::Batch { expiry_ms, packets } => {
                let packages = packets
                    .iter()
                    .filter_map(|packet| self.tick_backrun_packet(engine, blockhash_or_nonce, &packet.as_packet()))
                    .collect();
                Some(RelayerTick::Packages { expiry_ms, packages })
            }
        }
    }

    /// Profit in lamports and the SOL price written into the instruction.
    ///
    /// A WSOL base is already in lamports, so the price in the transaction stays 0.
    /// A stable base takes the price from config and the formula from the measurement: `profit · 1e9 / price`.
    /// A stable without a price builds no package — we do not treat USDC atoms as lamports.
    fn quote_lamports(&self, base: Pubkey, token_profit: u64) -> Option<(u64, u64)> {
        if base == self.cfg.wsol_mint {
            return Some((token_profit, 0));
        }
        if self.cfg.sol_price == 0 {
            return None;
        }
        let lamports = engine::encode::profit_in_lamports(token_profit, self.cfg.sol_price);
        if lamports == 0 { None } else { Some((lamports, self.cfg.sol_price)) }
    }

    /// Builds a package from a computed opportunity and route. Shared by 2-, 3-, 4- and 5-leg routes.
    fn build_package(
        &self,
        base: Pubkey,
        gross: u64,
        amount_in: u64,
        planned: PlannedRoute,
        blockhash_or_nonce: Pubkey,
        budget: Budget,
    ) -> Option<SignablePackage> {
        let (gross, sol_price) = self.quote_lamports(base, gross)?;
        let cfg = self.cfg;
        let auction = matches!(budget, Budget::Auction);
        // Lanes 5 and 6 pay the same bid as a priority fee. There is no tip.
        let priority_lane = auction && matches!(cfg.auction_lane, 5 | 6);

        let (tip, channel, tip_arg) = if priority_lane {
            (0, Channel::Auction, None)
        } else if auction {
            match (cfg.auction_frac_bps, cfg.tip_account) {
                (f, Some(dest)) if f > 0 => {
                    if gross < cfg.auction_min_e {
                        return None;
                    }
                    let t = bid::auction_tip(gross, f);
                    (t, Channel::Auction, Some((dest, t)))
                }
                _ => (0, Channel::PublicTpu, None),
            }
        } else {
            (0, Channel::PublicTpu, None)
        };

        // Auction with tip (lanes 2 and 4): the bid is the tip, CU price is 0, so B = tip.
        // Lanes 5 and 6: B = priority fee = the same bid, tip = 0.
        // Backrun: B = 0. A positive profit in lamports is enough. The auction requires a fee and a margin.
        let priority_fit = if priority_lane {
            if cfg.auction_frac_bps == 0 || gross < cfg.auction_min_e {
                return None;
            }
            let bid_fee = bid::auction_tip(gross, cfg.auction_frac_bps);
            let fit = priority::fit_exact_priority_fee(cfg.cu_limit, bid_fee)?;
            let fixed_cost = fee::total_fee(fit.cu_price_micro, fit.cu_limit, 1);
            if !bid::is_profitable(gross, fixed_cost, 0, cfg.min_margin) {
                return None;
            }
            Some((fit, fixed_cost, bid_fee))
        } else {
            None
        };
        let auction_budget = if auction && priority_fit.is_none() {
            let price = if tip > 0 { 0 } else { cfg.cu_price_micro };
            let priority = fee::priority_fee(price, cfg.cu_limit);
            let fixed_cost = fee::total_fee(price, cfg.cu_limit, 1);
            if !bid::is_profitable(gross, fixed_cost, tip, cfg.min_margin) {
                return None;
            }
            Some((cfg.cu_limit, price, fixed_cost, priority.saturating_add(tip)))
        } else {
            None
        };
        let b_threshold = priority_fit
            .as_ref()
            .map(|(_, _, b)| *b)
            .or_else(|| auction_budget.map(|(_, _, _, b)| b))
            .unwrap_or(0);

        // Route accounts: prefix + intermediate triples + legs; the invariant Σhop+3n+4 is enforced by assemble.
        let prefix = route::fixed_prefix(
            cfg.bot,
            cfg.state_pda,
            cfg.base_ata,
            cfg.native_bridge_pda,
            cfg.wsol_mint,
            cfg.rent_sysvar,
        );
        let declared: Vec<u8> = planned.plan_hops.iter().map(|h| h.n_accounts).collect();
        let route_accounts =
            route::assemble_route(&prefix, &planned.intermediates, &planned.hops, &declared).ok()?;

        // Point A: intermediate accounts (CreateIdempotent) inserted before the program in the same tx.
        let setup = if cfg.create_intermediate_accounts {
            crate::accounts::intermediate_setup_ixs(cfg.bot, &planned.intermediates)
        } else {
            Vec::new()
        };

        // LANE marks which builder assembled the route: 1 = public backrun, 2 = auction.
        // Does not build a bundle.
        let lane = match &budget {
            Budget::Backrun { .. } => 1,
            Budget::Auction if matches!(cfg.auction_lane, 4 | 5 | 6) => cfg.auction_lane,
            Budget::Auction => 2,
        };

        // Lane 5 (Harmonic) does not use a nonce in the measurement: 0/537.
        let use_nonce = cfg.use_durable_nonce && lane != 5;
        let nonce_account = if use_nonce {
            match cfg.nonce_pair {
                Some(pair) => keys::execution_nonce_pair(&cfg.bot, pair)?.0,
                None => cfg.nonce_account,
            }
        } else {
            cfg.nonce_account
        };

        let compile = |route_bytes: &[u8], cu_limit: u32, cu_price_micro: u64| -> (Vec<u8>, Blockhash) {
            let route_data = route_bytes.to_vec();
            if use_nonce {
                let msg = send::nonce::build_arb_message_with_nonce_and_setup(
                    cfg.bot,
                    blockhash_or_nonce,
                    nonce_account,
                    cfg.nonce_authority,
                    cfg.arb_program,
                    route_data,
                    route_accounts.clone(),
                    cu_limit,
                    cfg.loaded_accounts_data_bytes,
                    cu_price_micro,
                    tip_arg,
                    &setup,
                    &cfg.alts,
                );
                (msg, Blockhash::DurableNonce)
            } else {
                let msg = send::tx::build_arb_message_with_setup(
                    cfg.bot,
                    blockhash_or_nonce,
                    cfg.arb_program,
                    route_data,
                    route_accounts.clone(),
                    cu_limit,
                    cfg.loaded_accounts_data_bytes,
                    cu_price_micro,
                    tip_arg,
                    &setup,
                    &cfg.alts,
                );
                (msg, Blockhash::Recent)
            }
        };

        let (cu_limit, cu_price_micro, fixed_cost, route_data) = if let Budget::Backrun { target_rp } = budget {
            // The probe has X = 0. Its length is the same as the final route, so the fitted rp stays valid.
            let probe = encode_route(amount_in, b_threshold, &planned.plan_hops, &meta_with_price(lane, 0, sol_price, cfg.copy_index))?;
            let (probe_msg, _) = compile(&probe, cfg.cu_limit, 0);
            let shape = priority::shape_from_message(&probe_msg, u64::from(cfg.loaded_accounts_data_bytes))?;
            let fit = priority::fit_priority(shape, cfg.cu_limit, target_rp)?;
            let fixed_cost = fee::total_fee(fit.cu_price_micro, fit.cu_limit, 1);
            // B = 0: the program lets through any positive profit. `quote_lamports` already rejected zero.
            let net = gross.saturating_sub(fixed_cost).saturating_sub(tip) as i64;
            let route_data = encode_route(amount_in, b_threshold, &planned.plan_hops, &meta_with_price(lane, net, sol_price, cfg.copy_index))?;
            (fit.cu_limit, fit.cu_price_micro, fixed_cost, route_data)
        } else if let Some((fit, fixed_cost, _)) = priority_fit {
            let net = gross.saturating_sub(fixed_cost) as i64;
            let route_data = encode_route(amount_in, b_threshold, &planned.plan_hops, &meta_with_price(lane, net, sol_price, cfg.copy_index))?;
            (fit.cu_limit, fit.cu_price_micro, fixed_cost, route_data)
        } else {
            let (limit, price, fixed_cost, _) = auction_budget?;
            let net = gross.saturating_sub(fixed_cost).saturating_sub(tip) as i64;
            let route_data = encode_route(amount_in, b_threshold, &planned.plan_hops, &meta_with_price(lane, net, sol_price, cfg.copy_index))?;
            (limit, price, fixed_cost, route_data)
        };

        let (message, blockhash_mode) = compile(&route_data, cu_limit, cu_price_micro);

        Some(SignablePackage {
            message,
            fee_payer: cfg.bot,
            blockhash_mode,
            expected_gross: gross,
            fixed_cost,
            tip,
            channel,
            cu_limit,
            cu_price_micro,
        })
    }
}

fn meta_with_price(lane: u8, predicted_net: i64, sol_price: u64, copy_index: u8) -> RouteMeta {
    let mut meta = RouteMeta::new(lane, predicted_net);
    meta.sol_price = sol_price;
    meta.copy_index = copy_index;
    meta
}

/// Full live pass: fetch accounts → assemble engine → build package → (optionally) hand it
/// to the operator for signing and sending. `nonce_or_blockhash` is supplied by the caller (a fresh blockhash
/// from RPC or [`keys::durable_nonce`] from the last blockhash). Returns the package, if one was built.
pub fn poll_once(
    cfg: &RunnerConfig,
    builder: &dyn PoolBuilder,
    planner: &dyn RoutePlanner,
    src: &mut CurlSource,
    nonce_or_blockhash: Pubkey,
    submitter: &mut dyn Submitter,
) -> Result<Option<SignablePackage>, String> {
    src.prefetch(&builder.account_keys())?;
    let engine = build_engine(cfg, builder, src);
    let pkg = Runner { cfg, planner }.tick(&engine, nonce_or_blockhash);
    if let Some(p) = &pkg {
        if p.worth_sending(cfg.min_margin) {
            submitter.submit(p.clone());
        }
    }
    Ok(pkg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MapSource;

    fn route_ix_data(message: &[u8]) -> Vec<u8> {
        let mut raw = vec![1u8];
        raw.extend_from_slice(&[9u8; 64]);
        raw.extend_from_slice(message);
        let ours = crate::pending::decode_transaction(&raw).unwrap();
        ours.instructions.into_iter().find(|ix| ix.program == id(200)).unwrap().data
    }

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    fn cfg(use_nonce: bool, min_margin: u64) -> RunnerConfig {
        RunnerConfig {
            base: id(1),
            bot: id(100),
            arb_program: id(200),
            state_pda: id(2),
            base_ata: id(3),
            native_bridge_pda: id(4),
            wsol_mint: id(1),
            rent_sysvar: id(6),
            max_input: 500_000,
            min_profit: 0,
            sol_price: 0,
            min_margin,
            cu_limit: 127_000,
            cu_price_micro: 61_825,
            loaded_accounts_data_bytes: 131_072,
            auction_frac_bps: bid::OBSERVED_AUCTION_FRAC_BPS,
            auction_min_e: 0,
            auction_lane: 2,
            tip_account: Some(id(240)),
            create_intermediate_accounts: true,
            use_durable_nonce: use_nonce,
            nonce_account: id(50),
            nonce_authority: id(100), // = bot → 1 signature
            alts: Vec::new(),
            nonce_pair: None,
            copy_index: 0,
        }
    }

    /// Two CP pools on the pair (base, T) with a divergence → 2-leg opportunity (like the engine test).
    fn engine_with_arb() -> Engine {
        let mut e = Engine::new(id(1), 500_000, 0);
        e.add_pool(Pool {
            id: id(10),
            token_a: id(1),
            token_b: id(2),
            kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 2_000_000, fee_bps: 30 },
        });
        e.add_pool(Pool {
            id: id(11),
            token_a: id(1),
            token_b: id(2),
            kind: engine::Kind::Cp { res_a: 2_000_000, res_b: 1_000_000, fee_bps: 30 },
        });
        e
    }

    /// Stub planner: two legs of 3 accounts each (non-signer), intermediate account 1 triple. Satisfies
    /// the assemble_route invariant and lets the whole byte-building path run without DEX layouts.
    struct StubPlanner;
    impl StubPlanner {
        fn route() -> PlannedRoute {
            let ro = |n: u8| AccountMeta { pubkey: id(n), is_signer: false, is_writable: false };
            PlannedRoute {
                intermediates: vec![[ro(70), ro(71), ro(72)]],
                hops: vec![vec![ro(80), ro(81), ro(82)], vec![ro(90), ro(91), ro(92)]],
                plan_hops: vec![
                    PlanHop { venue: 0x03, direction: 0, n_accounts: 3, token_account_size: 0 },
                    PlanHop { venue: 0x03, direction: 0, n_accounts: 3, token_account_size: 0 },
                ],
            }
        }
    }
    impl RoutePlanner for StubPlanner {
        fn plan_two_hop(&self, _: &Opportunity) -> Option<PlannedRoute> {
            Some(Self::route())
        }
        fn plan_three_hop(&self, _: &TriOpportunity) -> Option<PlannedRoute> {
            None
        }
    }

    fn stub_hops(n: usize) -> PlannedRoute {
        let ro = |k: u8| AccountMeta { pubkey: id(k), is_signer: false, is_writable: false };
        let mut intermediates = Vec::new();
        let mut hops = Vec::new();
        let mut plan_hops = Vec::new();
        for i in 0..n {
            let b = 80 + (i as u8) * 10;
            hops.push(vec![ro(b), ro(b + 1), ro(b + 2)]);
            plan_hops.push(PlanHop { venue: 0x03, direction: 0, n_accounts: 3, token_account_size: 0 });
            if i + 1 != n {
                let t = 20 + (i as u8) * 3;
                intermediates.push([ro(t), ro(t + 1), ro(t + 2)]);
            }
        }
        PlannedRoute { intermediates, hops, plan_hops }
    }

    struct CyclePlanner {
        hops: usize,
    }

    impl RoutePlanner for CyclePlanner {
        fn plan_two_hop(&self, _: &Opportunity) -> Option<PlannedRoute> {
            Some(stub_hops(2))
        }
        fn plan_three_hop(&self, _: &TriOpportunity) -> Option<PlannedRoute> {
            None
        }
        fn plan_cycle(&self, opp: &CycleOpportunity) -> Option<PlannedRoute> {
            if opp.pools.len() == self.hops {
                Some(stub_hops(self.hops))
            } else {
                None
            }
        }
    }

    fn doubling_cycle(n_hops: u8) -> Engine {
        let mut e = Engine::new(id(1), 100_000, 0);
        for i in 0..n_hops {
            let from = if i == 0 { 1 } else { i + 1 };
            let to = if i + 1 == n_hops { 1 } else { i + 2 };
            e.add_pool(Pool {
                id: id(10 + i),
                token_a: id(from),
                token_b: id(to),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 2_000_000, fee_bps: 30 },
            });
        }
        e
    }

    #[test]
    fn four_and_five_hop_cycles_encode_measured_lengths() {
        let planner = CyclePlanner { hops: 4 };
        let runner = Runner { cfg: &cfg(false, 1), planner: &planner };
        let pkg = runner.tick(&doubling_cycle(4), id(222)).expect("4 hops");
        let data = route_ix_data(&pkg.message);
        assert_eq!(data.len(), 117);
        assert_eq!(data[0], 0x0a);
        assert_eq!(data[25], 4);
        assert_eq!(data[62 + 20 + 1], 2, "auction writes LANE 2");
        assert_eq!(pkg.cu_price_micro, 0);
        assert!(pkg.tip > 0);

        let victim = victim_tx(80_000);
        let back = runner.tick_backrun(&doubling_cycle(4), id(222), &victim).expect("4-hop backrun");
        let back_data = route_ix_data(&back.message);
        assert_eq!(back_data.len(), 117);
        assert_eq!(back_data[62 + 20 + 1], 1, "backrun stays on LANE 1");
        assert_eq!(back.tip, 0);
        assert_eq!(back.channel, Channel::PublicTpu);
        assert_eq!(&back_data[9..17], &0u64.to_le_bytes());

        let five = CyclePlanner { hops: 5 };
        let runner5 = Runner { cfg: &cfg(false, 1), planner: &five };
        let pkg5 = runner5.tick(&doubling_cycle(5), id(222)).expect("5 hops");
        let data5 = route_ix_data(&pkg5.message);
        assert_eq!(data5.len(), 122);
        assert_eq!(data5[25], 5);
    }

    #[test]
    fn longer_cycle_without_a_planner_keeps_the_two_hop() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut e = engine_with_arb();
        for pool in doubling_cycle(4).pools {
            e.add_pool(pool);
        }
        assert!(e.find_cycle(4)[0].profit > e.find_two_hop()[0].profit);
        let pkg = runner.tick(&e, id(222)).expect("the 2-hop stays");
        let data = route_ix_data(&pkg.message);
        assert_eq!(data.len(), 107);
        assert_eq!(data[25], 2);
    }

    #[test]
    fn tick_builds_signable_package_with_nonce() {
        let cfg = cfg(true, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let nonce_value = keys::durable_nonce(&id(222));
        let pkg = runner.tick(&engine_with_arb(), nonce_value).expect("a package should be built");

        assert!(pkg.expected_gross > 0);
        assert_eq!(pkg.fee_payer, id(100));
        assert_eq!(pkg.blockhash_mode, Blockhash::DurableNonce);
        assert_eq!(pkg.channel, Channel::Auction);
        assert_eq!(pkg.tip, bid::auction_tip(pkg.expected_gross, bid::OBSERVED_AUCTION_FRAC_BPS));
        assert!(pkg.worth_sending(1));
        // v0 message, one signature (nonce authority = bot).
        assert_eq!(pkg.message[0], 0x80, "v0 message");
        assert_eq!(pkg.message[1], 1, "one signature");
        // Nonce value in the message's recent_blockhash field.
        assert!(pkg.message.windows(32).any(|w| w == nonce_value), "nonce in message");
        // AdvanceNonce as the first ix (data [4,0,0,0] present).
        assert!(pkg.message.windows(4).any(|w| w == [4, 0, 0, 0]));
        let route = route_ix_data(&pkg.message);
        assert_eq!(route[0], 0x0a);
        assert_eq!(route.len(), 107);
        assert_eq!(route[73], 2, "auction writes LANE 2");
        let net = pkg.expected_gross.saturating_sub(pkg.fixed_cost).saturating_sub(pkg.tip) as i64;
        assert_eq!(i64::from_le_bytes(route[83..91].try_into().unwrap()), net);
        assert_eq!(pkg.cu_price_micro, 0, "auction with tip has CU price 0");
        assert_eq!(pkg.fixed_cost, 5_000);
        assert_eq!(u64::from_le_bytes(route[9..17].try_into().unwrap()), pkg.tip, "B = bid");
        assert_eq!(net as u64 + pkg.tip + 5_000, pkg.expected_gross);
    }

    #[test]
    fn copy_index_is_written_at_the_trailer() {
        let mut cfg = cfg(true, 1);
        cfg.copy_index = 5;
        let planner = StubPlanner;
        let pkg = Runner { cfg: &cfg, planner: &planner }
            .tick(&engine_with_arb(), keys::durable_nonce(&id(222)))
            .expect("package");
        let route = route_ix_data(&pkg.message);
        assert_eq!(route[75], 5, "COPY at n=2 sits at trailer+3");
        assert_eq!(route[73], 2);
    }

    #[test]
    fn auction_below_measured_floor_builds_nothing() {
        let mut c = cfg(false, 1);
        c.auction_min_e = bid::MIN_AUCTION_E;
        let planner = StubPlanner;
        assert!(Runner { cfg: &c, planner: &planner }.tick(&engine_with_arb(), id(222)).is_none());
    }

    #[test]
    fn stable_base_prices_the_bid_in_lamports() {
        let engine = engine_with_arb();
        let opp = engine.find_two_hop().into_iter().next().unwrap();
        let price = 121_296_691u64;
        let lamports = engine::encode::profit_in_lamports(opp.profit, price);
        assert_ne!(lamports, opp.profit);
        assert!(lamports > 0);

        let mut stable = cfg(false, 0);
        stable.wsol_mint = id(9);
        stable.sol_price = price;
        let planner = StubPlanner;
        let runner = Runner { cfg: &stable, planner: &planner };
        let pkg = runner.tick(&engine, id(222)).expect("stable base");
        let data = route_ix_data(&pkg.message);
        assert_eq!(u64::from_le_bytes(data[17..25].try_into().unwrap()), price);
        assert_eq!(u64::from_le_bytes(data[1..9].try_into().unwrap()), opp.amount_in);
        assert_eq!(pkg.expected_gross, lamports);
        assert_eq!(pkg.tip, bid::auction_tip(lamports, bid::OBSERVED_AUCTION_FRAC_BPS));
        assert_eq!(u64::from_le_bytes(data[9..17].try_into().unwrap()), pkg.tip);
        let net = lamports.saturating_sub(pkg.fixed_cost).saturating_sub(pkg.tip) as i64;
        assert_eq!(i64::from_le_bytes(data[83..91].try_into().unwrap()), net);

        let mut missing = stable.clone();
        missing.sol_price = 0;
        assert!(Runner { cfg: &missing, planner: &planner }.tick(&engine, id(222)).is_none());

        let mut wsol = cfg(false, 0);
        wsol.sol_price = price;
        let plain = Runner { cfg: &wsol, planner: &planner }.tick(&engine, id(222)).unwrap();
        let plain_data = route_ix_data(&plain.message);
        assert_eq!(&plain_data[17..25], &[0u8; 8], "WSOL base leaves price 0");
        assert_eq!(plain.expected_gross, opp.profit);

        let back = runner.tick_backrun(&engine, id(222), &victim_tx(80_000)).expect("stable backrun");
        let back_data = route_ix_data(&back.message);
        assert_eq!(u64::from_le_bytes(back_data[17..25].try_into().unwrap()), price);
        assert_eq!(&back_data[9..17], &0u64.to_le_bytes());
        assert_eq!(back.tip, 0);
        assert_eq!(back.channel, Channel::PublicTpu);
        assert_eq!(back_data[73], 1);
        assert_eq!(back.expected_gross, lamports);
    }

    #[test]
    fn harmonic_and_bam_pay_the_bid_as_a_priority_fee() {
        let engine = engine_with_arb();
        let planner = StubPlanner;
        let mut harmonic = cfg(true, 0);
        harmonic.auction_lane = 5;
        let pkg = Runner { cfg: &harmonic, planner: &planner }.tick(&engine, id(222)).expect("lane 5");
        let data = route_ix_data(&pkg.message);
        let bid_fee = bid::auction_tip(pkg.expected_gross, bid::OBSERVED_AUCTION_FRAC_BPS);
        assert_eq!(data[73], 5);
        assert_eq!(pkg.tip, 0);
        assert_eq!(pkg.channel, Channel::Auction);
        assert_eq!(pkg.blockhash_mode, Blockhash::Recent, "Harmonic is always on a blockhash");
        assert!(!pkg.message.windows(32).any(|w| w == id(240)), "no tip account");
        assert_eq!(fee::priority_fee(pkg.cu_price_micro, pkg.cu_limit), bid_fee);
        assert_eq!(u64::from_le_bytes(data[9..17].try_into().unwrap()), bid_fee);
        assert_eq!(pkg.fixed_cost, 5_000 + bid_fee);
        let net = pkg.expected_gross.saturating_sub(pkg.fixed_cost) as i64;
        assert_eq!(i64::from_le_bytes(data[83..91].try_into().unwrap()), net);
        assert_eq!(net as u64 + bid_fee + 5_000, pkg.expected_gross);

        let mut bam = cfg(true, 0);
        bam.auction_lane = 6;
        let bam_pkg = Runner { cfg: &bam, planner: &planner }.tick(&engine, id(222)).expect("lane 6");
        let bam_data = route_ix_data(&bam_pkg.message);
        assert_eq!(bam_data[73], 6);
        assert_eq!(bam_pkg.tip, 0);
        assert_eq!(bam_pkg.blockhash_mode, Blockhash::DurableNonce);
        assert_eq!(
            fee::priority_fee(bam_pkg.cu_price_micro, bam_pkg.cu_limit),
            bid::auction_tip(bam_pkg.expected_gross, bid::OBSERVED_AUCTION_FRAC_BPS)
        );

        let mut helius = cfg(false, 0);
        helius.auction_lane = 4;
        let helius_pkg = Runner { cfg: &helius, planner: &planner }.tick(&engine, id(222)).unwrap();
        assert_eq!(route_ix_data(&helius_pkg.message)[73], 4);
        assert!(helius_pkg.tip > 0);
        assert_eq!(helius_pkg.cu_price_micro, 0);
        assert_eq!(
            u64::from_le_bytes(route_ix_data(&helius_pkg.message)[9..17].try_into().unwrap()),
            helius_pkg.tip
        );

        let mut other = cfg(false, 0);
        other.auction_lane = 9;
        let tip_pkg = Runner { cfg: &other, planner: &planner }.tick(&engine, id(222)).unwrap();
        assert_eq!(route_ix_data(&tip_pkg.message)[73], 2);
        assert!(tip_pkg.tip > 0);
        assert_eq!(tip_pkg.cu_price_micro, 0);

        harmonic.auction_min_e = bid::MIN_AUCTION_E;
        assert!(Runner { cfg: &harmonic, planner: &planner }.tick(&engine, id(222)).is_none());
    }

    #[test]
    fn intermediate_account_creation_in_message() {
        // Point A: with create_intermediate_accounts=true the message contains CreateIdempotent ATA
        // (ATA program present), and with false — it does not (and is shorter).
        let planner = StubPlanner;
        let mut c = cfg(false, 1); // fresh blockhash → simpler message
        c.create_intermediate_accounts = true;
        let on = Runner { cfg: &c, planner: &planner }.tick(&engine_with_arb(), id(222)).unwrap();
        assert!(
            on.message.windows(32).any(|w| w == crate::accounts::ATA_PROGRAM),
            "ATA program present = intermediate accounts setup inserted"
        );

        c.create_intermediate_accounts = false;
        let off = Runner { cfg: &c, planner: &planner }.tick(&engine_with_arb(), id(222)).unwrap();
        assert!(
            !off.message.windows(32).any(|w| w == crate::accounts::ATA_PROGRAM),
            "without setup: no ATA program in message"
        );
        assert!(off.message.len() < on.message.len(), "without setup the message is shorter");
    }

    #[test]
    fn supplied_alt_leaves_static_keys_and_nonce_pair_keeps_the_even_account() {
        let mut c = cfg(true, 1);
        let moved = id(70);
        c.alts = vec![send::tx::AddressLookupTable { address: id(210), addresses: vec![moved] }];
        c.nonce_pair = Some(0);
        let (even, odd) = keys::execution_nonce_pair(&c.bot, 0).unwrap();
        let pkg = Runner { cfg: &c, planner: &StubPlanner }
            .tick(&engine_with_arb(), id(222))
            .expect("package with ALT and nonce pair");
        assert_eq!(pkg.message[0], 0x80);
        assert_eq!(pkg.message[1], 1);
        assert!(!pkg.message.windows(32).any(|w| w == moved), "account from ALT is not a static key");
        assert!(pkg.message.windows(32).any(|w| w == id(210)), "table address is in the message");
        assert!(pkg.message.windows(32).any(|w| w == even), "even account of the pair is in the message");
        assert!(!pkg.message.windows(32).any(|w| w == odd), "odd partner does not enter the transaction");
        assert!(!pkg.message.windows(32).any(|w| w == id(50)), "nonce_account from config yields to the pair");
        let mut raw = vec![1u8];
        raw.extend_from_slice(&[9u8; 64]);
        raw.extend_from_slice(&pkg.message);
        let decoded = crate::pending::decode_transaction(&raw).unwrap();
        assert_eq!(decoded.unresolved_lookups, 1);
        assert!(!decoded.static_keys.contains(&moved));
    }

    fn victim_tx(price: u64) -> crate::pending::DecodedTx {
        crate::pending::decode_transaction(&crate::pending::cpmm_exact_in_legacy(id(1), id(2), 1, price)).unwrap()
    }

    fn package_priority(message: &[u8]) -> u64 {
        let mut raw = vec![1u8];
        raw.extend_from_slice(&[9u8; 64]);
        raw.extend_from_slice(message);
        let tx = crate::pending::decode_transaction(&raw).unwrap();
        crate::pending::scheduler_priority(&tx).unwrap()
    }

    #[test]
    fn tick_backrun_sorts_after_the_victim_without_a_tip() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let victim = victim_tx(80_000);
        let victim_rp = crate::pending::scheduler_priority(&victim).unwrap();
        let pkg = runner.tick_backrun(&engine_with_arb(), id(222), &victim).expect("backrun");
        assert_eq!(package_priority(&pkg.message), victim_rp - 1);
        assert_eq!(pkg.message[0], 0x80, "backrun is v0 too");
        assert_eq!(pkg.message[1], 1);
        assert_eq!(pkg.tip, 0);
        assert_eq!(pkg.channel, Channel::PublicTpu);
        let mut no_limit = victim.clone();
        no_limit.cu_limit = None;
        assert!(runner.tick_backrun(&engine_with_arb(), id(222), &no_limit).is_none());
    }

    #[test]
    fn backrun_appears_only_after_the_victim_swap() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut calm = Engine::new(id(1), 500_000, 0);
        calm.add_pool(Pool {
            id: id(10),
            token_a: id(1),
            token_b: id(2),
            kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
        });
        calm.add_pool(Pool {
            id: id(11),
            token_a: id(1),
            token_b: id(2),
            kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
        });
        let victim = victim_tx(2);
        assert!(runner.tick_backrun(&calm, id(222), &victim).is_none());
        let pkg = runner
            .tick_backrun_after(
                &calm,
                id(222),
                &victim,
                engine::VictimSwap { pool: id(10), a_to_b: true, amount_in: 400_000 },
            )
            .expect("arbitrage appears after the swap");
        assert_eq!(package_priority(&pkg.message), crate::pending::scheduler_priority(&victim).unwrap() - 1);
        assert_eq!(pkg.tip, 0);
        assert!(pkg.expected_gross > 0);
        assert!(calm.find_two_hop().is_empty());
    }

    #[test]
    fn tick_backrun_tx_reads_the_pending_cpmm_swap() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut calm = Engine::new(id(1), 500_000, 0);
        for pid in [10u8, 11] {
            calm.add_pool(Pool {
                id: id(pid),
                token_a: id(1),
                token_b: id(2),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
            });
        }
        assert!(calm.find_two_hop().is_empty());

        let raw = crate::pending::cpmm_exact_in_legacy(id(1), id(2), 400_000, 2);
        let tx = crate::pending::decode_transaction(&raw).unwrap();
        let pkg = runner.tick_backrun_tx(&calm, id(222), &tx).expect("arbitrage from instruction");
        assert_eq!(package_priority(&pkg.message), crate::pending::scheduler_priority(&tx).unwrap() - 1);
        assert_eq!(pkg.tip, 0);
        assert_eq!(pkg.channel, Channel::PublicTpu);
        assert!(pkg.expected_gross > 0);
        let ours = {
            let mut raw = vec![1u8];
            raw.extend_from_slice(&[9u8; 64]);
            raw.extend_from_slice(&pkg.message);
            crate::pending::decode_transaction(&raw).unwrap()
        };
        let route = ours.instructions.iter().find(|ix| ix.program == id(200)).unwrap();
        assert_eq!(&route.data[9..17], &0u64.to_le_bytes(), "threshold B on the backrun is 0");
        assert_eq!(route.data[0], 0x0a);
        assert_eq!(route.data.len(), 107);
        assert_eq!(&route.data[77..81], b"MRXP");
        assert_eq!(route.data[73], 1, "backrun writes LANE 1");
        let net = pkg.expected_gross.saturating_sub(pkg.fixed_cost).saturating_sub(pkg.tip) as i64;
        assert_eq!(i64::from_le_bytes(route.data[83..91].try_into().unwrap()), net);
        assert!(calm.find_two_hop().is_empty(), "registry stays as before the swap");

        let rev = crate::pending::decode_transaction(&crate::pending::cpmm_exact_in_legacy(id(2), id(1), 400_000, 2)).unwrap();
        let swaps = crate::pending::victim_swaps(&rev, &calm.pools);
        assert_eq!(swaps, vec![engine::VictimSwap { pool: id(10), a_to_b: false, amount_in: 400_000 }]);
        let pkg_b = runner.tick_backrun_tx(&calm, id(222), &rev).expect("direction b→a");
        assert_eq!(package_priority(&pkg_b.message), crate::pending::scheduler_priority(&rev).unwrap() - 1);
        assert_eq!(pkg_b.tip, 0);
        assert!(pkg_b.expected_gross > 0);

        let mut bare = tx.clone();
        bare.instructions.clear();
        assert!(runner.tick_backrun_tx(&calm, id(222), &bare).is_none());
        bare.instructions = tx.instructions.clone();
        bare.cu_limit = None;
        assert!(runner.tick_backrun_tx(&calm, id(222), &bare).is_none());
        assert!(calm.find_two_hop().is_empty());
    }

    #[test]
    fn tick_backrun_tx_reads_a_v1_cpmm_swap() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut calm = Engine::new(id(1), 500_000, 0);
        for pid in [10u8, 11] {
            calm.add_pool(Pool {
                id: id(pid),
                token_a: id(1),
                token_b: id(2),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
            });
        }
        let raw = crate::pending::cpmm_exact_in_v1(id(1), id(2), 400_000, Some(1), Some(200_000), Some(131_072), None);
        let tx = crate::pending::decode_transaction(&raw).unwrap();
        let pkg = runner.tick_backrun_tx(&calm, id(222), &tx).expect("arbitrage from v1");
        assert_eq!(package_priority(&pkg.message), crate::pending::scheduler_priority(&tx).unwrap() - 1);
        assert_eq!(pkg.tip, 0);
        assert_eq!(pkg.channel, Channel::PublicTpu);
        assert!(pkg.expected_gross > 0);
        let ours = {
            let mut raw = vec![1u8];
            raw.extend_from_slice(&[9u8; 64]);
            raw.extend_from_slice(&pkg.message);
            crate::pending::decode_transaction(&raw).unwrap()
        };
        let route = ours.instructions.iter().find(|ix| ix.program == id(200)).unwrap();
        assert_eq!(&route.data[9..17], &0u64.to_le_bytes(), "threshold B on the backrun is 0");
        assert!(calm.find_two_hop().is_empty());

        let mut no_loaded = tx.clone();
        no_loaded.loaded_accounts_bytes = None;
        assert!(runner.tick_backrun_tx(&calm, id(222), &no_loaded).is_none());
    }

    #[test]
    fn tick_backrun_packet_reads_one_relayer_packet() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut calm = Engine::new(id(1), 500_000, 0);
        for pid in [10u8, 11] {
            calm.add_pool(Pool {
                id: id(pid),
                token_a: id(1),
                token_b: id(2),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
            });
        }

        let raw = crate::pending::cpmm_exact_in_legacy(id(1), id(2), 400_000, 2);
        let tx = crate::pending::decode_transaction(&raw).unwrap();
        let from_tx = runner.tick_backrun_tx(&calm, id(222), &tx).unwrap();
        let packet = crate::pending::Packet { bytes: &raw, discard: false, simple_vote: false };
        let pkg = runner.tick_backrun_packet(&calm, id(222), &packet).expect("arbitrage from packet");
        assert_eq!(pkg.message, from_tx.message);
        assert_eq!(pkg.tip, 0);
        assert_eq!(pkg.channel, Channel::PublicTpu);
        assert_eq!(pkg.expected_gross, from_tx.expected_gross);
        assert!(pkg.expected_gross > 0);
        assert_eq!(package_priority(&pkg.message), crate::pending::scheduler_priority(&tx).unwrap() - 1);
        let route = {
            let mut wrapped = vec![1u8];
            wrapped.extend_from_slice(&[9u8; 64]);
            wrapped.extend_from_slice(&pkg.message);
            let ours = crate::pending::decode_transaction(&wrapped).unwrap();
            ours.instructions.into_iter().find(|ix| ix.program == id(200)).unwrap()
        };
        assert_eq!(&route.data[9..17], &0u64.to_le_bytes(), "threshold B on the backrun is 0");

        let v1 = crate::pending::cpmm_exact_in_v1(id(1), id(2), 400_000, Some(1), Some(200_000), Some(131_072), None);
        let v1_tx = crate::pending::decode_transaction(&v1).unwrap();
        let v1_packet = crate::pending::Packet { bytes: &v1, discard: false, simple_vote: false };
        let pkg_v1 = runner.tick_backrun_packet(&calm, id(222), &v1_packet).expect("arbitrage from v1 packet");
        assert_eq!(package_priority(&pkg_v1.message), crate::pending::scheduler_priority(&v1_tx).unwrap() - 1);
        assert_eq!(pkg_v1.tip, 0);
        assert_eq!(pkg_v1.channel, Channel::PublicTpu);

        assert!(runner.tick_backrun_packet(&calm, id(222), &crate::pending::Packet { bytes: &raw, discard: true, simple_vote: false }).is_none());
        assert!(runner.tick_backrun_packet(&calm, id(222), &crate::pending::Packet { bytes: &raw, discard: false, simple_vote: true }).is_none());
        assert!(runner.tick_backrun_packet(&calm, id(222), &crate::pending::Packet { bytes: &[0x82], discard: false, simple_vote: false }).is_none());
        assert!(calm.find_two_hop().is_empty(), "registry stays as before the swap");
    }

    #[test]
    fn tick_backrun_update_reads_a_relayer_batch() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let mut calm = Engine::new(id(1), 500_000, 0);
        for pid in [10u8, 11] {
            calm.add_pool(Pool {
                id: id(pid),
                token_a: id(1),
                token_b: id(2),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
            });
        }
        let raw = crate::pending::cpmm_exact_in_legacy(id(1), id(2), 400_000, 2);
        let tx = crate::pending::decode_transaction(&raw).unwrap();
        let from_tx = runner.tick_backrun_tx(&calm, id(222), &tx).unwrap();
        let mut padded = raw.clone();
        padded.extend_from_slice(&[0xAA, 0xBB]);
        let body = crate::relayer::encode_packet_batch_for_test(&[
            crate::relayer::TestPacket {
                data: &padded,
                size: raw.len() as u64,
                discard: false,
                simple_vote: false,
                addr: "10.0.0.8:8003",
            },
            crate::relayer::TestPacket {
                data: &raw,
                size: raw.len() as u64,
                discard: true,
                simple_vote: false,
                addr: "",
            },
            crate::relayer::TestPacket {
                data: &raw,
                size: raw.len() as u64 + 8,
                discard: false,
                simple_vote: false,
                addr: "",
            },
        ]);
        let tick = runner.tick_backrun_update(&calm, id(222), &body).expect("batch");
        let RelayerTick::Packages { expiry_ms, packages } = tick else { panic!("expected batch") };
        assert_eq!(expiry_ms, 157);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].message, from_tx.message);
        assert_eq!(packages[0].tip, 0);
        assert_eq!(packages[0].channel, Channel::PublicTpu);
        assert_eq!(package_priority(&packages[0].message), crate::pending::scheduler_priority(&tx).unwrap() - 1);
        let route = {
            let mut wrapped = vec![1u8];
            wrapped.extend_from_slice(&[9u8; 64]);
            wrapped.extend_from_slice(&packages[0].message);
            let ours = crate::pending::decode_transaction(&wrapped).unwrap();
            ours.instructions.into_iter().find(|ix| ix.program == id(200)).unwrap()
        };
        assert_eq!(&route.data[9..17], &0u64.to_le_bytes(), "threshold B on the backrun is 0");
        let n = route.data[25] as usize;
        assert_eq!(route.data[62 + 5 * n + 1], 1, "LANE byte stays in the race");

        let beat = crate::relayer::encode_heartbeat_for_test(4);
        match runner.tick_backrun_update(&calm, id(222), &beat).unwrap() {
            RelayerTick::Heartbeat { count } => assert_eq!(count, 4),
            RelayerTick::Packages { .. } => panic!("heartbeat builds no route"),
        }
        assert!(runner.tick_backrun_update(&calm, id(222), &[0x0a]).is_none());
        assert!(calm.find_two_hop().is_empty(), "registry stays as before the swap");
    }

    #[test]
    fn tick_recent_blockhash_mode() {
        let cfg = cfg(false, 1);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let bh = id(222);
        let pkg = runner.tick(&engine_with_arb(), bh).expect("package");
        assert_eq!(pkg.blockhash_mode, Blockhash::Recent);
        assert!(pkg.message.windows(32).any(|w| w == bh), "fresh blockhash in message");
        // Without AdvanceNonce: we do not insert a System instruction with the nonce tag at the start — we check
        // that the instruction count is 1 lower than in nonce mode (indirectly: no nonce account).
        assert!(!pkg.message.windows(32).any(|w| w == id(50)), "no nonce account in normal mode");
    }

    #[test]
    fn unprofitable_opportunity_yields_no_package() {
        // Margin threshold unreachable → no auction package, though the opportunity exists.
        let cfg = cfg(true, u64::MAX);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        assert!(runner.tick(&engine_with_arb(), keys::durable_nonce(&id(1))).is_none());
    }

    #[test]
    fn backrun_keeps_a_positive_profit_below_the_margin() {
        // B = 0: a positive profit in lamports suffices, even when it covers neither the fee nor the margin.
        let cfg = cfg(false, u64::MAX);
        let planner = StubPlanner;
        let runner = Runner { cfg: &cfg, planner: &planner };
        let pkg = runner.tick_backrun(&engine_with_arb(), id(222), &victim_tx(80_000)).expect("backrun");
        assert!(pkg.expected_gross > 0);
        assert_eq!(pkg.tip, 0);
        assert!(!pkg.worth_sending(cfg.min_margin));
        assert!(runner.tick(&engine_with_arb(), id(222)).is_none(), "auction stays bound by the margin");
    }

    #[test]
    fn no_opportunity_yields_no_package() {
        // Balanced pools → no opportunity.
        let mut e = Engine::new(id(1), 500_000, 0);
        for pid in [10u8, 11] {
            e.add_pool(Pool {
                id: id(pid),
                token_a: id(1),
                token_b: id(2),
                kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
            });
        }
        let cfg = cfg(true, 1);
        let planner = StubPlanner;
        assert!(Runner { cfg: &cfg, planner: &planner }.tick(&e, id(9)).is_none());
    }

    /// Stub builder: two CP pools with a divergence, assembled from "accounts" in MapSource (here directly).
    struct StubBuilder;
    impl PoolBuilder for StubBuilder {
        fn account_keys(&self) -> Vec<Pubkey> {
            vec![id(10), id(11)]
        }
        fn build_pools(&self, _src: &dyn AccountSource) -> Vec<Pool> {
            vec![
                Pool {
                    id: id(10),
                    token_a: id(1),
                    token_b: id(2),
                    kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 2_000_000, fee_bps: 30 },
                },
                Pool {
                    id: id(11),
                    token_a: id(1),
                    token_b: id(2),
                    kind: engine::Kind::Cp { res_a: 2_000_000, res_b: 1_000_000, fee_bps: 30 },
                },
            ]
        }
    }

    #[test]
    fn build_engine_then_tick_full_pipeline() {
        // Full core path without a network: builder → engine → tick → package, plus the Submitter seam.
        let cfg = cfg(true, 1);
        let src = MapSource::new();
        let e = build_engine(&cfg, &StubBuilder, &src);
        let planner = StubPlanner;
        let pkg = Runner { cfg: &cfg, planner: &planner }.tick(&e, keys::durable_nonce(&id(7)));
        let pkg = pkg.expect("package from the full path");

        let mut sub = DryRunSubmitter::default();
        if pkg.worth_sending(cfg.min_margin) {
            sub.submit(pkg);
        }
        assert_eq!(sub.packages.len(), 1, "DryRunSubmitter collected the package, sent nothing");
        assert!(sub.packages[0].expected_gross > 0);
    }
}
