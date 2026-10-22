//! On-chain program: arbitrage route executor with **rollback when there is no profit**.
//!
//! Principle: the program is a thin
//! route interpreter. The off-chain engine sends an encoded route; the program:
//! 1. records the base token balance BEFORE,
//! 2. executes the legs in turn (CPI into the target DEX programs) with `min_out ≤ 1` per leg,
//! 3. records the balance AFTER,
//! 4. **if profit ≤ threshold → returns an error**, so the whole transaction rolls back (no state
//!    is written), and the cost is limited to the fee.
//!
//! This way a missed off-chain quote never causes a capital loss — at worst the
//! transaction reverts. This is the safety layer of the whole bot.
//!
//! This file holds the **dependency-free, tested core**: route parsing and the profit threshold.
//! The actual SBF entrypoint (CPI into the DEXes, reading balances from accounts) arrives with the build against the
//! Solana SDK — sketch in [`entrypoint_sketch`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod cpi;
pub mod executor;

/// Errors returned by the program (mapped to `Custom(u32)` when the transaction is rolled back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArbError {
    /// Profit ≤ 0 — the route did not come out positive (`Custom(0)`).
    NoProfit,
    /// Profit positive but below the threshold (fees/tip > profit). The code carries `profit/1000`.
    BelowThreshold(u64),
    /// Malformed instruction data format.
    BadRoute,
    /// Caller is not authorised (state PDA mismatch / wrong owner). → `Custom(6902)`.
    Unauthorized,
}

/// One leg of the route. `token_account_size` is u16 LE; 0 means the token account already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hop {
    pub venue: u8,
    pub direction: u8,
    pub n_accounts: u8,
    pub token_account_size: u16,
}

/// Route header read from the instruction data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteHeader {
    pub amount_in: u64,
    /// Minimum required profit in the base token (total tx cost: fee + tip). Below → revert.
    pub min_profit: u64,
    /// SOL price, 6 decimal places. 0 keeps profit in balance units (WSOL base).
    /// A non-zero price converts stable profit to lamports: `profit · 1e9 / price`.
    pub sol_price: u64,
    pub n_hops: u8,
    /// `trailer[0]`. A value of 1 sets the flag read by the parser. The builder writes 0.
    pub trailer_flag: u8,
}

/// Instruction fields the program does not read. The builder leaves zeros apart from `lane` and `predicted_net`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteMeta {
    pub sol_price: u64,
    pub nonce: u32,
    pub k: [u8; 32],
    pub lane: u8,
    pub t2: u8,
    pub copy_index: u8,
    pub sig_type: u8,
    pub trailer_flag: u8,
    pub mrxp_flag: u8,
    pub predicted_net: i64,
    pub extra_u64: u64,
    pub salt: [u8; 8],
}

impl RouteMeta {
    pub fn new(lane: u8, predicted_net: i64) -> Self {
        Self {
            sol_price: 0,
            nonce: 0,
            k: [0u8; 32],
            lane,
            t2: 0,
            copy_index: 0,
            sig_type: 0,
            trailer_flag: 0,
            mrxp_flag: 0,
            predicted_net,
            extra_u64: 0,
            salt: [0u8; 8],
        }
    }
}

/// Instruction tag "initialise state PDA" (`Initialize`, tag 0x00).
pub const TAG_INITIALIZE: u8 = 0x00;
/// Instruction tag "execute route". The reference program rejects tag `0x01`.
pub const TAG_EXECUTE: u8 = 0x0a;

/// Authorisation error code (`Custom(6902)`).
pub const UNAUTHORIZED_CODE: u32 = 6902;

/// State account layout (state PDA): `[1, bump, authority(32)]` — 34 B.
pub const STATE_LEN: usize = 34;

/// Checks authorisation from the state account data and the caller's key.
/// Requires: length ≥ 34, init byte `data[0]==1`, `data[2..34]==authority`.
pub fn check_authority(state_data: &[u8], authority: &[u8; 32]) -> Result<(), ArbError> {
    if state_data.len() < STATE_LEN || state_data[0] != 1 || &state_data[2..34] != authority {
        return Err(ArbError::Unauthorized);
    }
    Ok(())
}

/// Start of the leg descriptors. Before them: tag, amount, B, SOL price, leg count, nonce and K.
pub const HOPS_OFF: usize = 62;
/// Size of one leg descriptor: venue, direction, account count, token account size u16 LE.
pub const HOP_LEN: usize = 5;
/// Tail after the hops: flag, LANE, t2, COPY, SIG.
pub const TRAILER_LEN: usize = 5;
/// MRXP block appended by the encoder. The parser does not require it.
pub const MRXP_LEN: usize = 30;
pub const MRXP_MAGIC: [u8; 4] = *b"MRXP";
pub const MRXP_VERSION: u8 = 2;

/// Length of the full instruction with the MRXP tail: `97 + 5·n`.
pub const fn route_len(n_hops: u8) -> usize {
    HOPS_OFF + n_hops as usize * HOP_LEN + TRAILER_LEN + MRXP_LEN
}

/// Parses the route header. Requires tag `0x0a`, `n` in `2..=6` and the trailer flag byte.
/// The MRXP magic is not checked: the program does not read it.
pub fn parse_header(data: &[u8]) -> Result<RouteHeader, ArbError> {
    if data.len() < HOPS_OFF || data[0] != TAG_EXECUTE {
        return Err(ArbError::BadRoute);
    }
    let n_hops = data[25];
    if !(2..=6).contains(&n_hops) {
        return Err(ArbError::BadRoute);
    }
    let trailer = HOPS_OFF + n_hops as usize * HOP_LEN;
    if data.len() <= trailer {
        return Err(ArbError::BadRoute);
    }
    Ok(RouteHeader {
        amount_in: u64::from_le_bytes(copy8(&data[1..9])),
        min_profit: u64::from_le_bytes(copy8(&data[9..17])),
        sol_price: u64::from_le_bytes(copy8(&data[17..25])),
        n_hops,
        trailer_flag: data[trailer],
    })
}

/// Returns the i-th leg of the route.
pub fn hop(data: &[u8], i: u8) -> Result<Hop, ArbError> {
    let header = parse_header(data)?;
    if i >= header.n_hops {
        return Err(ArbError::BadRoute);
    }
    let off = HOPS_OFF + i as usize * HOP_LEN;
    if off + HOP_LEN > data.len() {
        return Err(ArbError::BadRoute);
    }
    Ok(Hop {
        venue: data[off],
        direction: data[off + 1],
        n_accounts: data[off + 2],
        token_account_size: u16::from_le_bytes([data[off + 3], data[off + 4]]),
    })
}

/// Encodes the full `0x0a` instruction together with the trailer and MRXP. `n` outside `2..=6` → [`ArbError::BadRoute`].
pub fn encode_route(amount_in: u64, min_profit: u64, hops: &[Hop], meta: &RouteMeta) -> Result<Vec<u8>, ArbError> {
    if !(2..=6).contains(&hops.len()) {
        return Err(ArbError::BadRoute);
    }
    let n = hops.len() as u8;
    let mut d = Vec::new();
    d.resize(route_len(n), 0);
    d[0] = TAG_EXECUTE;
    d[1..9].copy_from_slice(&amount_in.to_le_bytes());
    d[9..17].copy_from_slice(&min_profit.to_le_bytes());
    d[17..25].copy_from_slice(&meta.sol_price.to_le_bytes());
    d[25] = n;
    d[26..30].copy_from_slice(&meta.nonce.to_le_bytes());
    d[30..62].copy_from_slice(&meta.k);
    for (i, h) in hops.iter().enumerate() {
        let off = HOPS_OFF + i * HOP_LEN;
        d[off] = h.venue;
        d[off + 1] = h.direction;
        d[off + 2] = h.n_accounts;
        d[off + 3..off + 5].copy_from_slice(&h.token_account_size.to_le_bytes());
    }
    let trailer = HOPS_OFF + hops.len() * HOP_LEN;
    d[trailer] = meta.trailer_flag;
    d[trailer + 1] = meta.lane;
    d[trailer + 2] = meta.t2;
    d[trailer + 3] = meta.copy_index;
    d[trailer + 4] = meta.sig_type;
    let mrxp = trailer + TRAILER_LEN;
    d[mrxp..mrxp + 4].copy_from_slice(&MRXP_MAGIC);
    d[mrxp + 4] = MRXP_VERSION;
    d[mrxp + 5] = meta.mrxp_flag;
    d[mrxp + 6..mrxp + 14].copy_from_slice(&meta.predicted_net.to_le_bytes());
    d[mrxp + 14..mrxp + 22].copy_from_slice(&meta.extra_u64.to_le_bytes());
    d[mrxp + 22..mrxp + 30].copy_from_slice(&meta.salt);
    Ok(d)
}

/// **Safety core**: checks profit after the route has executed.
///
/// `before` / `after` are the bot wallet's base token balance before and after the route.
/// `sol_price` = 0 compares the balance directly (WSOL base). A non-zero price, 6 decimal
/// places, converts stable profit to lamports using the formula from the measurement: `profit · 1e9 / price`.
/// Threshold B is in lamports. Lamport profit ≤ 0 → [`ArbError::NoProfit`].
/// Positive profit but ≤ B → [`ArbError::BelowThreshold`] with code `profit/1000`.
pub fn check_profit(before: u64, after: u64, min_profit: u64) -> Result<u64, ArbError> {
    check_profit_with_price(before, after, min_profit, 0)
}

/// Like [`check_profit`], with the SOL price from the route header.
pub fn check_profit_with_price(before: u64, after: u64, min_profit: u64, sol_price: u64) -> Result<u64, ArbError> {
    if after <= before {
        return Err(ArbError::NoProfit);
    }
    let profit = profit_in_lamports(after - before, sol_price);
    if profit == 0 {
        return Err(ArbError::NoProfit);
    }
    if profit <= min_profit {
        return Err(ArbError::BelowThreshold(profit / 1000));
    }
    Ok(profit)
}

/// Profit in lamports. Price 0 keeps balance units. Otherwise `token_profit · 1_000_000_000 / sol_price`.
pub fn profit_in_lamports(token_profit: u64, sol_price: u64) -> u64 {
    if sol_price == 0 {
        token_profit
    } else {
        u64::try_from(u128::from(token_profit).saturating_mul(1_000_000_000) / u128::from(sol_price)).unwrap_or(u64::MAX)
    }
}

fn copy8(s: &[u8]) -> [u8; 8] {
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    a
}

/// Sketch of the SBF entrypoint — to be completed when building against the Solana SDK.
///
/// ```text
/// fn process_instruction(program_id, accounts, data):
///     h = parse_header(data)?                 // tag 0x0a, n w 2..=6
///     base_ta = accounts[BASE_TOKEN_ACCOUNT_IDX]
///     before = spl_token_amount(base_ta)
///     let mut acc_cursor = FIRST_HOP_ACCOUNT_IDX
///     for i in 0..h.n_hops:
///         let hop = hop(data, i)?
///         let hop_accounts = &accounts[acc_cursor .. acc_cursor + hop.n_accounts]
///         cpi_swap(hop.venue, hop.direction, hop_accounts, min_out = 1)?   // no slippage on the leg
///         acc_cursor += hop.n_accounts
///     after = spl_token_amount(base_ta)
///     check_profit_with_price(before, after, h.min_profit, h.sol_price)?
///     Ok(())
/// ```
pub mod entrypoint_sketch {}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_hops() -> [Hop; 2] {
        [
            Hop { venue: 0x03, direction: 0, n_accounts: 15, token_account_size: 0 },
            Hop { venue: 0x01, direction: 1, n_accounts: 18, token_account_size: 165 },
        ]
    }

    #[test]
    fn roundtrip_header_and_hops() {
        let hops = two_hops();
        let mut meta = RouteMeta::new(1, 2_596_311);
        meta.sol_price = 121_296_691;
        meta.trailer_flag = 1;
        meta.nonce = 0xAABB_CCDD;
        meta.k = [9u8; 32];
        let data = encode_route(448_245_643, 0, &hops, &meta).unwrap();
        assert_eq!(data.len(), 107);
        assert_eq!(route_len(2), 107);
        assert_eq!(route_len(3), 112);
        assert_eq!(route_len(4), 117);
        assert_eq!(route_len(5), 122);

        let h = parse_header(&data).unwrap();
        assert_eq!(h.amount_in, 448_245_643);
        assert_eq!(h.min_profit, 0);
        assert_eq!(h.sol_price, 121_296_691);
        assert_eq!(h.n_hops, 2);
        assert_eq!(h.trailer_flag, 1);
        assert_eq!(hop(&data, 0).unwrap(), hops[0]);
        assert_eq!(hop(&data, 1).unwrap(), hops[1]);
        assert_eq!(hop(&data, 2), Err(ArbError::BadRoute));

        assert_eq!(data[0], TAG_EXECUTE);
        assert_eq!(&data[9..17], &0u64.to_le_bytes());
        assert_eq!(data[62], 0x03);
        assert_eq!(u16::from_le_bytes([data[65], data[66]]), 0);
        assert_eq!(data[67], 0x01);
        assert_eq!(u16::from_le_bytes([data[70], data[71]]), 165);
        assert_eq!(data[72], 1);
        assert_eq!(data[73], 1);
        assert_eq!(&data[77..81], b"MRXP");
        assert_eq!(data[81], MRXP_VERSION);
        assert_eq!(i64::from_le_bytes(data[83..91].try_into().unwrap()), 2_596_311);

        // Nonce, K and the rest of the trailer do not change how the legs are read.
        let mut other = meta;
        other.nonce = 0;
        other.k = [0u8; 32];
        other.t2 = 4;
        other.copy_index = 5;
        other.sig_type = 7;
        let bare = encode_route(448_245_643, 0, &hops, &other).unwrap();
        assert_eq!(hop(&bare, 0).unwrap(), hops[0]);
        assert_eq!(hop(&bare, 1).unwrap(), hops[1]);
        assert_eq!(parse_header(&bare).unwrap().trailer_flag, 1);
    }

    #[test]
    fn rejects_truncated_wrong_tag_and_hop_count() {
        let hops = two_hops();
        let data = encode_route(1, 0, &hops, &RouteMeta::new(1, -1)).unwrap();
        assert_eq!(i64::from_le_bytes(data[83..91].try_into().unwrap()), -1);
        let min = HOPS_OFF + 2 * HOP_LEN + 1;
        assert_eq!(parse_header(&data[..min - 1]), Err(ArbError::BadRoute));
        let short = parse_header(&data[..min]).unwrap();
        assert_eq!(short.n_hops, 2);
        assert_eq!(hop(&data[..min], 1).unwrap().token_account_size, 165);

        let mut flipped = data.clone();
        flipped[77] = 0;
        assert!(parse_header(&flipped).is_ok(), "MRXP magic is not required");

        assert_eq!(parse_header(&[0x0a, 0, 0]), Err(ArbError::BadRoute));
        let mut old_tag = data.clone();
        old_tag[0] = 0x01;
        assert_eq!(parse_header(&old_tag), Err(ArbError::BadRoute));
        assert_eq!(parse_header(&[0x02; 120]), Err(ArbError::BadRoute));

        for n in [0u8, 1, 7, 255] {
            let mut d = vec![0u8; 200];
            d[0] = TAG_EXECUTE;
            d[25] = n;
            assert_eq!(parse_header(&d), Err(ArbError::BadRoute), "n={n}");
        }
        assert!(encode_route(1, 0, &hops[..1], &RouteMeta::new(1, 0)).is_err());
        let seven = [hops[0]; 7];
        assert!(encode_route(1, 0, &seven, &RouteMeta::new(1, 0)).is_err());

        let six = [hops[0]; 6];
        let wide = Hop { venue: 0x08, direction: 0, n_accounts: 15, token_account_size: 0x01A5 };
        let mut six = six;
        six[5] = wide;
        let data6 = encode_route(1, 0, &six, &RouteMeta::new(1, 0)).unwrap();
        assert_eq!(data6.len(), route_len(6));
        assert_eq!(hop(&data6, 5).unwrap().token_account_size, 0x01A5);
    }

    #[test]
    fn profit_guard() {
        assert_eq!(check_profit(1000, 1100, 50), Ok(100)); // profit 100 > threshold 50
        assert_eq!(check_profit(1000, 1000, 0), Err(ArbError::NoProfit));
        assert_eq!(check_profit(1000, 900, 0), Err(ArbError::NoProfit));
        // profit 40 ≤ threshold 50 → revert with code profit/1000 = 0
        assert_eq!(check_profit(1000, 1040, 50), Err(ArbError::BelowThreshold(0)));
        // profit 2500 ≤ threshold 3000 → code 2
        assert_eq!(check_profit(0, 2500, 3000), Err(ArbError::BelowThreshold(2)));
    }

    #[test]
    fn profit_exactly_at_threshold_reverts() {
        // profit == threshold → revert (we require strictly greater, to cover the cost).
        assert_eq!(check_profit(1000, 1050, 50), Err(ArbError::BelowThreshold(0)));
        // one more → passes.
        assert_eq!(check_profit(1000, 1051, 50), Ok(51));
    }

    #[test]
    fn stable_base_converts_token_profit_by_the_sol_price() {
        // 1_000_000 base units (1 USDC at 6 dp) at a price of 121.296691 USD, also 6 dp.
        let price = 121_296_691u64;
        let lamp = profit_in_lamports(1_000_000, price);
        assert_eq!(lamp, 8_244_247);
        assert_eq!(check_profit_with_price(0, 1_000_000, lamp - 1, price), Ok(lamp));
        assert_eq!(check_profit_with_price(0, 1_000_000, lamp, price), Err(ArbError::BelowThreshold(lamp / 1000)));
        // Price 0 keeps the balance. WSOL does not enter the division.
        assert_eq!(check_profit_with_price(0, 1_000_000, 0, 0), Ok(1_000_000));
        // Division down to zero lamports is no profit, not threshold B.
        assert_eq!(check_profit_with_price(0, 1, 0, u64::MAX), Err(ArbError::NoProfit));
    }

    #[test]
    fn authority_check() {
        let bot = [7u8; 32];
        let mut state = [0u8; STATE_LEN];
        state[0] = 1; // initialised
        state[1] = 253; // bump
        state[2..34].copy_from_slice(&bot);
        assert_eq!(check_authority(&state, &bot), Ok(()));
        // different authority → Unauthorized
        assert_eq!(check_authority(&state, &[9u8; 32]), Err(ArbError::Unauthorized));
        // uninitialised (data[0] != 1) → Unauthorized
        let mut uninit = state;
        uninit[0] = 0;
        assert_eq!(check_authority(&uninit, &bot), Err(ArbError::Unauthorized));
        // too short → Unauthorized
        assert_eq!(check_authority(&[1u8; 10], &bot), Err(ArbError::Unauthorized));
    }
}
