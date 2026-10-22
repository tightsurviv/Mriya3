//! Pre-execution transactions from the private orderflow relayer (Relayer/TPU).
//!
//! The relayer is a gRPC client. It pushes packets shaped like Jito's
//! `Packet` to our endpoint (`data` = raw Solana transaction, `meta.size` = how many bytes are valid).
//! This module decodes the payload and says which known pools and DEX programs the instructions
//! touch. It does not build sandwiches and does not insert transactions before the user's transaction.
//!
//! The gRPC server (counterpart of `jito-labs/block_engine_simple`) is a separate process:
//! this crate does not pull in `tonic`. The `PacketBatchUpdate` body is decoded by [`crate::relayer`]
//! and truncated to `meta.size`. The handler calls [`scan`] on those bytes.
//! The identity of the relayer that auth should let in is configured by the gRPC server.

use keys::venue_pda;
use send::route::{damm_v1, damm_v2, ray_v4};

/// One TPU packet after truncation to `meta.size`.
pub struct Packet<'a> {
    pub bytes: &'a [u8],
    pub discard: bool,
    pub simple_vote: bool,
}

/// Transaction decoded to the extent needed for the arbitrage signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedTx {
    /// First signature (transaction identifier).
    pub signature: [u8; 64],
    /// Static message keys, in header order.
    pub static_keys: Vec<[u8; 32]>,
    /// Programs invoked by the instructions (resolved from static keys).
    pub program_keys: Vec<[u8; 32]>,
    pub versioned: bool,
    /// Number of ALT entries in a v0 message. The addresses from the table are not here.
    pub unresolved_lookups: u8,
    /// Number of signatures in the transaction.
    pub n_signatures: u8,
    /// Write locks: static keys to write plus ALT write entries.
    /// An ALT address is not deduplicated here against a static key.
    pub n_write_locks: u16,
    /// Last `SetComputeUnitLimit` or the CU limit from the v1 config.
    /// `None` when the victim does not set it.
    pub cu_limit: Option<u32>,
    /// Last `SetLoadedAccountsDataSizeLimit` or the account data limit from the v1 config.
    /// `None` on legacy/v0 → the scheduler takes 64 MiB. On v1 a missing field stays `None`:
    /// the default 0 B does not let the transaction into a block.
    pub loaded_accounts_bytes: Option<u32>,
    /// Last `SetComputeUnitPrice` (µlamport/CU). Always `None` on v1:
    /// the fee sits in the config as an amount in lamports.
    pub cu_price_micro: Option<u64>,
    /// A v1 transaction (SIMD-0385). The budget is in the config, not in ComputeBudget instructions.
    pub v1: bool,
    /// v1 priority fee in lamports. `Some(0)` when the fee bits are off.
    /// Always `None` on legacy and v0.
    pub priority_fee_lamports: Option<u64>,
    /// Message instructions. An account index points into [`Self::static_keys`].
    /// An index outside that list is an ALT entry and cannot be resolved here.
    pub instructions: Vec<Instruction>,
}

/// One message instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub program: [u8; 32],
    pub account_indexes: Vec<u8>,
    pub data: Vec<u8>,
}

impl Instruction {
    /// Account key at position `i` in this instruction, if it is among the static keys.
    pub fn account(&self, static_keys: &[[u8; 32]], i: usize) -> Option<[u8; 32]> {
        let idx = *self.account_indexes.get(i)? as usize;
        static_keys.get(idx).copied()
    }
}

/// CU price strictly below the victim's price. This is not the scheduler priority.
/// The backrun ordering is computed by [`scheduler_priority`] and `send::priority::fit_priority`.
pub fn backrun_cu_price(victim_micro_lamports: u64) -> Option<u64> {
    victim_micro_lamports.checked_sub(1)
}

/// Default account data limit when the transaction does not call `SetLoadedAccountsDataSizeLimit`.
pub const DEFAULT_LOADED_ACCOUNTS_BYTES: u32 = 64 * 1024 * 1024;

/// Agave scheduler priority of this transaction.
///
/// Legacy and v0: `None` without `SetComputeUnitLimit`. A missing account data limit counts as 64 MiB,
/// and the fee follows from the CU price. v1: the config fee is an amount in lamports, with no conversion
/// by price. `None` when the CU limit or the account data limit is omitted or equal to 0
/// (such a transaction will not enter a block; we do not substitute 64 MiB here).
pub fn scheduler_priority(tx: &DecodedTx) -> Option<u64> {
    let cu_limit = tx.cu_limit?;
    let (loaded, fee) = if tx.v1 {
        if cu_limit == 0 {
            return None;
        }
        let loaded = tx.loaded_accounts_bytes.filter(|n| *n > 0)?;
        let prio = tx.priority_fee_lamports.unwrap_or(0);
        let fee = send::fee::BASE_FEE_PER_SIGNATURE.checked_mul(u64::from(tx.n_signatures))?.checked_add(prio)?;
        (u64::from(loaded), fee)
    } else {
        let loaded = u64::from(tx.loaded_accounts_bytes.unwrap_or(DEFAULT_LOADED_ACCOUNTS_BYTES));
        let fee = send::fee::total_fee(tx.cu_price_micro.unwrap_or(0), cu_limit, u32::from(tx.n_signatures));
        (loaded, fee)
    };
    let shape = send::priority::TxShape {
        n_signatures: u64::from(tx.n_signatures),
        n_write_locks: u64::from(tx.n_write_locks),
        ix_data_bytes: tx.instructions.iter().map(|ix| ix.data.len() as u64).sum(),
        loaded_accounts_bytes: loaded,
    };
    let cost = send::priority::execution_cost(shape, u64::from(cu_limit));
    Some(send::priority::scheduler_priority(fee, u64::from(tx.n_signatures), cost))
}

/// Backrun ordering read from the pending transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackrunOrder {
    pub victim_signature: [u8; 64],
    pub cu_price_micro: u64,
}

/// Builds the backrun ordering. `None` when the victim has no CU price or the price is 0
/// (it is not possible to go below it).
pub fn backrun_order(tx: &DecodedTx) -> Option<BackrunOrder> {
    Some(BackrunOrder {
        victim_signature: tx.signature,
        cu_price_micro: backrun_cu_price(tx.cu_price_micro?)?,
    })
}

/// What in the pending transaction matches the bot's registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub signature: [u8; 64],
    pub pools: Vec<[u8; 32]>,
    pub programs: Vec<[u8; 32]>,
}

/// DEX programs that the bot can price or build.
pub fn watched_dex_programs() -> [[u8; 32]; 11] {
    [
        venue_pda::DLMM,
        venue_pda::CLMM,
        venue_pda::CPMM,
        venue_pda::WHIRLPOOL,
        venue_pda::MANIFEST,
        venue_pda::PUMP,
        damm_v2::PROGRAM,
        venue_pda::PANCAKE,
        venue_pda::BYREAL,
        damm_v1::PROGRAM,
        ray_v4::PROGRAM,
    ]
}

/// Decodes a legacy, v0 or v1 transaction. `None` on truncation, a bad version or an absurd size.
///
/// v1 (first byte `0x81`) has signatures at the end and the budget in the config. The rest goes the old path:
/// signature count first, then the message.
pub fn decode_transaction(bytes: &[u8]) -> Option<DecodedTx> {
    if bytes.first() == Some(&0x81) {
        return decode_v1(bytes);
    }
    let (sig_count, mut i) = read_compact(bytes, 0, 12)?;
    if sig_count == 0 {
        return None;
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(bytes.get(i..i + 64)?);
    i += 64 * sig_count;

    let first = *bytes.get(i)?;
    let versioned = first & 0x80 != 0;
    if versioned {
        if first & 0x7f != 0 {
            return None;
        }
        i += 1;
    }
    let num_ro_signed = *bytes.get(i + 1)? as usize;
    let num_ro_unsigned = *bytes.get(i + 2)? as usize;
    i += 3;

    let (n_keys, j) = read_compact(bytes, i, 64)?;
    i = j;
    if n_keys == 0 || num_ro_signed + num_ro_unsigned > n_keys {
        return None;
    }
    let mut n_write_locks = n_keys - num_ro_signed - num_ro_unsigned;
    let mut static_keys = Vec::with_capacity(n_keys);
    for _ in 0..n_keys {
        let mut key = [0u8; 32];
        key.copy_from_slice(bytes.get(i..i + 32)?);
        static_keys.push(key);
        i += 32;
    }
    i = i.checked_add(32)?;
    if i > bytes.len() {
        return None;
    }

    let (n_ix, j) = read_compact(bytes, i, 64)?;
    i = j;
    let mut program_keys = Vec::new();
    let mut cu_price_micro = None;
    let mut cu_limit = None;
    let mut loaded_accounts_bytes = None;
    let mut instructions = Vec::with_capacity(n_ix);
    for _ in 0..n_ix {
        let program_index = *bytes.get(i)? as usize;
        i += 1;
        let program = *static_keys.get(program_index)?;
        if !program_keys.contains(&program) {
            program_keys.push(program);
        }
        let (n_acc, j) = read_compact(bytes, i, 64)?;
        if j + n_acc > bytes.len() {
            return None;
        }
        let account_indexes = bytes[j..j + n_acc].to_vec();
        i = j + n_acc;
        let (n_data, j) = read_compact(bytes, i, 1232)?;
        let data_end = j + n_data;
        if data_end > bytes.len() {
            return None;
        }
        let data = bytes[j..data_end].to_vec();
        if program == send::tx::COMPUTE_BUDGET {
            if data.first() == Some(&send::compute_budget::SET_CU_PRICE) && data.len() >= 9 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&data[1..9]);
                cu_price_micro = Some(u64::from_le_bytes(buf));
            } else if data.first() == Some(&send::compute_budget::SET_CU_LIMIT) && data.len() >= 5 {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(&data[1..5]);
                cu_limit = Some(u32::from_le_bytes(buf));
            } else if data.first() == Some(&send::compute_budget::SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT) && data.len() >= 5
            {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(&data[1..5]);
                loaded_accounts_bytes = Some(u32::from_le_bytes(buf));
            }
        }
        instructions.push(Instruction { program, account_indexes, data });
        i = data_end;
    }

    let mut unresolved_lookups = 0u8;
    if versioned {
        let (n_lookup, j) = read_compact(bytes, i, 20)?;
        i = j;
        if n_lookup > u8::MAX as usize {
            return None;
        }
        unresolved_lookups = n_lookup as u8;
        for _ in 0..n_lookup {
            i = i.checked_add(32)?;
            let (n_w, j) = read_compact(bytes, i, 64)?;
            n_write_locks = n_write_locks.saturating_add(n_w);
            i = j + n_w;
            let (n_r, j) = read_compact(bytes, i, 64)?;
            i = j + n_r;
            if i > bytes.len() {
                return None;
            }
        }
    }

    let n_write_locks = u16::try_from(n_write_locks).ok()?;
    let n_signatures = u8::try_from(sig_count).ok()?;
    Some(DecodedTx {
        signature,
        static_keys,
        program_keys,
        versioned,
        unresolved_lookups,
        n_signatures,
        n_write_locks,
        cu_limit,
        loaded_accounts_bytes,
        cu_price_micro,
        v1: false,
        priority_fee_lamports: None,
        instructions,
    })
}

/// v1 transaction: version at the start, inline addresses, budget in a mask, signatures at the tail.
fn decode_v1(bytes: &[u8]) -> Option<DecodedTx> {
    const MAX_BYTES: usize = 4096;
    const MAX_SIGNATURES: usize = 12;
    const MAX_ACCOUNTS: usize = 64;
    const MAX_INSTRUCTIONS: usize = 64;
    const KNOWN_BITS: u32 = 0b1_1111;
    if bytes.len() > MAX_BYTES || bytes.first() != Some(&0x81) {
        return None;
    }
    let mut i = 1usize;
    let n_sig = *bytes.get(i)? as usize;
    let ro_signed = *bytes.get(i + 1)? as usize;
    let ro_unsigned = *bytes.get(i + 2)? as usize;
    i += 3;
    if n_sig == 0 || n_sig > MAX_SIGNATURES || ro_signed >= n_sig {
        return None;
    }
    let mask = u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?);
    i += 4;
    if mask & !KNOWN_BITS != 0 || (mask & 0b11) == 0b01 || (mask & 0b11) == 0b10 {
        return None;
    }
    i = i.checked_add(32)?;
    if i > bytes.len() {
        return None;
    }
    let n_ix = *bytes.get(i)? as usize;
    let n_addr = *bytes.get(i + 1)? as usize;
    i += 2;
    if n_ix > MAX_INSTRUCTIONS || n_addr == 0 || n_addr > MAX_ACCOUNTS || n_addr < n_sig + ro_unsigned {
        return None;
    }
    let mut static_keys = Vec::with_capacity(n_addr);
    for _ in 0..n_addr {
        let mut key = [0u8; 32];
        key.copy_from_slice(bytes.get(i..i + 32)?);
        if static_keys.contains(&key) {
            return None;
        }
        static_keys.push(key);
        i += 32;
    }

    let priority_fee_lamports = if mask & 0b11 == 0b11 {
        let fee = u64::from_le_bytes(bytes.get(i..i + 8)?.try_into().ok()?);
        i += 8;
        Some(fee)
    } else {
        Some(0)
    };
    let cu_limit = if mask & (1 << 2) != 0 {
        let v = u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?);
        i += 4;
        Some(v)
    } else {
        None
    };
    let loaded_accounts_bytes = if mask & (1 << 3) != 0 {
        let v = u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?);
        i += 4;
        Some(v)
    } else {
        None
    };
    if mask & (1 << 4) != 0 {
        let heap = u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?);
        i += 4;
        if !(32 * 1024u32..=256 * 1024).contains(&heap) || heap % 1024 != 0 {
            return None;
        }
    }

    let mut headers = Vec::with_capacity(n_ix);
    for _ in 0..n_ix {
        let program = *bytes.get(i)? as usize;
        let n_acc = *bytes.get(i + 1)? as usize;
        let n_data = u16::from_le_bytes(bytes.get(i + 2..i + 4)?.try_into().ok()?) as usize;
        i += 4;
        if program >= n_addr {
            return None;
        }
        headers.push((program, n_acc, n_data));
    }
    let mut program_keys = Vec::new();
    let mut instructions = Vec::with_capacity(n_ix);
    for (program_index, n_acc, n_data) in headers {
        let acc_end = i.checked_add(n_acc)?;
        let account_indexes = bytes.get(i..acc_end)?.to_vec();
        if account_indexes.iter().any(|idx| *idx as usize >= n_addr) {
            return None;
        }
        i = acc_end;
        let data_end = i.checked_add(n_data)?;
        let data = bytes.get(i..data_end)?.to_vec();
        i = data_end;
        let program = static_keys[program_index];
        if !program_keys.contains(&program) {
            program_keys.push(program);
        }
        instructions.push(Instruction { program, account_indexes, data });
    }

    let sig_end = i.checked_add(n_sig.checked_mul(64)?)?;
    if sig_end != bytes.len() {
        return None;
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&bytes[i..i + 64]);
    let n_write_locks = u16::try_from(n_addr.checked_sub(ro_signed)?.checked_sub(ro_unsigned)?).ok()?;
    Some(DecodedTx {
        signature,
        static_keys,
        program_keys,
        versioned: false,
        unresolved_lookups: 0,
        n_signatures: u8::try_from(n_sig).ok()?,
        n_write_locks,
        cu_limit,
        loaded_accounts_bytes,
        cu_price_micro: None,
        v1: true,
        priority_fee_lamports,
        instructions,
    })
}

/// Exact-in swaps from transactions whose pool is in the registry.
///
/// Reads the amount and pool from the instruction calling the exchange program. Direction:
/// input mint against `token_a` (CPMM, CLMM and Pancake `swap_v2`, Byreal), the `a_to_b` byte
/// (Whirlpool `swap` and `swap_v2`), sqrt MIN+1 / MAX−1 (CLMM and Pancake `swap` v1),
/// base sale (Pump sell) or quote purchase (Pump `buy_exact_quote_in`). Exact-out `buy` is skipped.
/// Manifest exact-in (`0x0d`, 19 B, last byte 1) takes the amount from offset 1 and `is_base_in` from byte 17.
/// DLMM, DAMM v1, DAMM v2 and Raydium v4 have no direction in the instruction data, so they are not here.
pub fn victim_swaps(tx: &DecodedTx, pools: &[engine::Pool]) -> Vec<engine::VictimSwap> {
    let mut out = Vec::new();
    for ix in &tx.instructions {
        if let Some(swap) = swap_from_ix(ix, &tx.static_keys, pools) {
            out.push(swap);
        }
    }
    out
}

fn swap_from_ix(ix: &Instruction, keys: &[[u8; 32]], pools: &[engine::Pool]) -> Option<engine::VictimSwap> {
    let pool = unique_pool(ix, keys, pools)?;
    if ix.program == venue_pda::MANIFEST {
        return manifest_swap(ix, pool);
    }
    let amount_in = u64_at(&ix.data, 8)?;
    if amount_in == 0 {
        return None;
    }
    let a_to_b = if ix.program == venue_pda::CPMM && ix.data.starts_with(&anchor("swap_base_input")) {
        let input = ix.account(keys, 10)?;
        direction(pool, input)?
    } else if (ix.program == venue_pda::CLMM || ix.program == venue_pda::PANCAKE)
        && ix.data.starts_with(&anchor("swap_v2"))
        && ix.data.get(40) == Some(&1)
    {
        let input = ix.account(keys, 11)?;
        direction(pool, input)?
    } else if (ix.program == venue_pda::CLMM || ix.program == venue_pda::PANCAKE)
        && ix.data.starts_with(&anchor("swap"))
        && ix.data.len() == 41
    {
        let sqrt = u128_at(&ix.data, 24)?;
        if sqrt == RAYDIUM_SQRT_MIN_PLUS_ONE {
            true
        } else if sqrt == RAYDIUM_SQRT_MAX_MINUS_ONE {
            false
        } else {
            return None;
        }
    } else if ix.program == venue_pda::BYREAL && ix.data.starts_with(&BYREAL_SWAP) {
        let input = ix.account(keys, 11)?;
        direction(pool, input)?
    } else if ix.program == venue_pda::WHIRLPOOL
        && ix.data.get(40) == Some(&1)
        && (ix.data.starts_with(&anchor("swap_v2")) || ix.data.starts_with(&anchor("swap")))
    {
        *ix.data.get(41)? == 1
    } else if ix.program == venue_pda::PUMP && ix.data.starts_with(&anchor("sell")) {
        let base = ix.account(keys, 3)?;
        if base != pool.token_a {
            return None;
        }
        true
    } else if ix.program == venue_pda::PUMP && ix.data.starts_with(&anchor("buy_exact_quote_in")) {
        let base = ix.account(keys, 3)?;
        if base != pool.token_a {
            return None;
        }
        false
    } else {
        return None;
    };
    Some(engine::VictimSwap { pool: pool.id, a_to_b, amount_in })
}

/// Tag `0x0d`, 19 B. The amount sits at offset 1, not 8: `out_atoms` is there.
/// Last byte 1 means exact-in. Byte 17 is `is_base_in` (1 = a→b).
fn manifest_swap(ix: &Instruction, pool: &engine::Pool) -> Option<engine::VictimSwap> {
    if ix.data.first() != Some(&0x0d) || ix.data.len() != 19 || ix.data.get(18) != Some(&1) {
        return None;
    }
    if ix.data[17] > 1 {
        return None;
    }
    let amount_in = u64_at(&ix.data, 1)?;
    if amount_in == 0 {
        return None;
    }
    Some(engine::VictimSwap { pool: pool.id, a_to_b: ix.data[17] == 1, amount_in })
}

fn direction(pool: &engine::Pool, input_mint: [u8; 32]) -> Option<bool> {
    if input_mint == pool.token_a {
        Some(true)
    } else if input_mint == pool.token_b {
        Some(false)
    } else {
        None
    }
}

fn unique_pool<'a>(ix: &Instruction, keys: &[[u8; 32]], pools: &'a [engine::Pool]) -> Option<&'a engine::Pool> {
    let mut found: Option<&engine::Pool> = None;
    for idx in &ix.account_indexes {
        // An index outside the static keys is an ALT entry. We leave it in the instruction
        // (IDL positions do not shift), but we cannot read a pool from it here.
        let Some(key) = keys.get(*idx as usize) else { continue };
        if let Some(pool) = pools.iter().find(|p| p.id == *key) {
            if let Some(prev) = found {
                if prev.id != pool.id {
                    return None;
                }
            } else {
                found = Some(pool);
            }
        }
    }
    found
}

fn u64_at(data: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?))
}

fn u128_at(data: &[u8], off: usize) -> Option<u128> {
    Some(u128::from_le_bytes(data.get(off..off + 16)?.try_into().ok()?))
}

/// Raydium and Pancake v1 sqrt-price. MIN+1 means a→b, MAX−1 means b→a.
const RAYDIUM_SQRT_MIN_PLUS_ONE: u128 = 4_295_048_017;
const RAYDIUM_SQRT_MAX_MINUS_ONE: u128 = 79_226_673_521_066_979_257_578_248_090;
/// Byreal discriminator from the window. It is not `sha256("global:swap")`.
const BYREAL_SWAP: [u8; 8] = [0xe5, 0x2e, 0xd5, 0x84, 0x69, 0x28, 0x28, 0xe4];

fn anchor(name: &str) -> [u8; 8] {
    let hash = keys::sha256(format!("global:{name}").as_bytes());
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

/// Returns a hit when any static key is a pool from the registry or an instruction calls an observed program.
pub fn hit(tx: &DecodedTx, pools: &[[u8; 32]], programs: &[[u8; 32]]) -> Option<Hit> {
    let pools_hit = tx.static_keys.iter().filter(|k| pools.contains(k)).copied().collect::<Vec<_>>();
    let programs_hit = tx.program_keys.iter().filter(|k| programs.contains(k)).copied().collect::<Vec<_>>();
    if pools_hit.is_empty() && programs_hit.is_empty() {
        return None;
    }
    Some(Hit { signature: tx.signature, pools: pools_hit, programs: programs_hit })
}

/// Skips discarded packets and votes. Decodes and filters the rest.
pub fn scan<'a>(packets: impl IntoIterator<Item = Packet<'a>>, pools: &[[u8; 32]], programs: &[[u8; 32]]) -> Vec<Hit> {
    let mut out = Vec::new();
    for packet in packets {
        if packet.discard || packet.simple_vote {
            continue;
        }
        let Some(tx) = decode_transaction(packet.bytes) else { continue };
        if let Some(h) = hit(&tx, pools, programs) {
            out.push(h);
        }
    }
    out
}

/// Legacy `swap_base_input` on Raydium CPMM.
///
/// 12 static keys. Position 3 of the instruction account list points to the pool,
/// position 10 — the input mint. The last account has an index outside the static list (ALT).
#[cfg(test)]
pub(crate) fn cpmm_exact_in_legacy(
    input_mint: [u8; 32],
    output_mint: [u8; 32],
    amount_in: u64,
    cu_price: u64,
) -> Vec<u8> {
    fn push_compact(buf: &mut Vec<u8>, mut n: usize) {
        loop {
            let mut b = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                b |= 0x80;
            }
            buf.push(b);
            if n == 0 {
                break;
            }
        }
    }
    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = n;
        k
    }

    let keys = [
        key(9),
        send::tx::COMPUTE_BUDGET,
        venue_pda::CPMM,
        key(20),
        key(21),
        key(10),
        key(30),
        key(31),
        key(32),
        key(33),
        input_mint,
        output_mint,
    ];
    // 13 IDL positions. 200 does not fit in the static keys.
    let accounts: [u8; 13] = [0, 3, 4, 5, 6, 7, 8, 9, 6, 7, 10, 11, 200];

    let mut buf = Vec::new();
    push_compact(&mut buf, 1);
    buf.extend_from_slice(&[0x44; 64]);
    buf.extend_from_slice(&[1, 0, 11]);
    push_compact(&mut buf, keys.len());
    for k in keys {
        buf.extend_from_slice(&k);
    }
    buf.extend_from_slice(&[0xAB; 32]);
    push_compact(&mut buf, 4);

    buf.push(1);
    push_compact(&mut buf, 0);
    let limit_ix = send::compute_budget::set_cu_limit(200_000);
    push_compact(&mut buf, limit_ix.len());
    buf.extend_from_slice(&limit_ix);

    buf.push(1);
    push_compact(&mut buf, 0);
    let loaded_ix = send::compute_budget::set_loaded_accounts_data_size_limit(131_072);
    push_compact(&mut buf, loaded_ix.len());
    buf.extend_from_slice(&loaded_ix);

    buf.push(1);
    push_compact(&mut buf, 0);
    let price_ix = send::compute_budget::set_cu_price(cu_price);
    push_compact(&mut buf, price_ix.len());
    buf.extend_from_slice(&price_ix);

    buf.push(2);
    push_compact(&mut buf, accounts.len());
    buf.extend_from_slice(&accounts);
    let mut data = anchor("swap_base_input").to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    push_compact(&mut buf, data.len());
    buf.extend_from_slice(&data);
    buf
}

/// Legacy Pump AMM leg. Accounts: 0 pool, 1 user, 2 global, 3 base, 4 quote.
/// `with_tail` appends byte `0x01`, just like `buy_exact_quote_in` in the window.
#[cfg(test)]
fn pump_leg_legacy(ix_name: &str, amount_in: u64, with_tail: bool) -> Vec<u8> {
    fn push_compact(buf: &mut Vec<u8>, mut n: usize) {
        loop {
            let mut b = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                b |= 0x80;
            }
            buf.push(b);
            if n == 0 {
                break;
            }
        }
    }
    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = n;
        k
    }
    let keys = [key(9), venue_pda::PUMP, key(10), key(40), key(41), key(1), key(2)];
    let accounts: [u8; 5] = [2, 3, 4, 5, 6];
    let mut buf = Vec::new();
    push_compact(&mut buf, 1);
    buf.extend_from_slice(&[0x44; 64]);
    buf.extend_from_slice(&[1, 0, 5]);
    push_compact(&mut buf, keys.len());
    for k in keys {
        buf.extend_from_slice(&k);
    }
    buf.extend_from_slice(&[0xAB; 32]);
    push_compact(&mut buf, 1);
    buf.push(1);
    push_compact(&mut buf, accounts.len());
    buf.extend_from_slice(&accounts);
    let mut data = anchor(ix_name).to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    if with_tail {
        data.push(1);
    }
    push_compact(&mut buf, data.len());
    buf.extend_from_slice(&data);
    buf
}

/// The same CPMM swap as [`cpmm_exact_in_legacy`], in a v1 envelope.
///
/// The budget is in the config. The instructions keep ComputeBudget with a different price — the decoder
/// must ignore it. `None` in a budget field turns off the corresponding mask bit.
#[cfg(test)]
pub(crate) fn cpmm_exact_in_v1(
    input_mint: [u8; 32],
    output_mint: [u8; 32],
    amount_in: u64,
    priority_fee_lamports: Option<u64>,
    cu_limit: Option<u32>,
    loaded_accounts_bytes: Option<u32>,
    heap_bytes: Option<u32>,
) -> Vec<u8> {
    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = n;
        k
    }
    let keys = [
        key(9),
        send::tx::COMPUTE_BUDGET,
        venue_pda::CPMM,
        key(20),
        key(21),
        key(10),
        key(30),
        key(31),
        key(32),
        key(33),
        input_mint,
        output_mint,
    ];
    // The last index points to the payer. There is no ALT in v1, so 200 would be outside the list.
    let accounts: [u8; 13] = [0, 3, 4, 5, 6, 7, 8, 9, 6, 7, 10, 11, 0];

    let mut mask = 0u32;
    if priority_fee_lamports.is_some() {
        mask |= 0b11;
    }
    if cu_limit.is_some() {
        mask |= 1 << 2;
    }
    if loaded_accounts_bytes.is_some() {
        mask |= 1 << 3;
    }
    if heap_bytes.is_some() {
        mask |= 1 << 4;
    }

    let mut buf = Vec::new();
    buf.push(0x81);
    buf.extend_from_slice(&[1, 0, 11]);
    buf.extend_from_slice(&mask.to_le_bytes());
    buf.extend_from_slice(&[0xAB; 32]);
    buf.push(4);
    buf.push(keys.len() as u8);
    for k in keys {
        buf.extend_from_slice(&k);
    }
    if let Some(fee) = priority_fee_lamports {
        buf.extend_from_slice(&fee.to_le_bytes());
    }
    if let Some(limit) = cu_limit {
        buf.extend_from_slice(&limit.to_le_bytes());
    }
    if let Some(loaded) = loaded_accounts_bytes {
        buf.extend_from_slice(&loaded.to_le_bytes());
    }
    if let Some(heap) = heap_bytes {
        buf.extend_from_slice(&heap.to_le_bytes());
    }

    let limit_ix = send::compute_budget::set_cu_limit(200_000);
    let loaded_ix = send::compute_budget::set_loaded_accounts_data_size_limit(131_072);
    let price_ix = send::compute_budget::set_cu_price(50_000);
    let mut data = anchor("swap_base_input").to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    for (program, n_acc, n_data) in [(1u8, 0u8, limit_ix.len()), (1, 0, loaded_ix.len()), (1, 0, price_ix.len()), (2, accounts.len() as u8, data.len())] {
        buf.push(program);
        buf.push(n_acc);
        buf.extend_from_slice(&(n_data as u16).to_le_bytes());
    }
    buf.extend_from_slice(&limit_ix);
    buf.extend_from_slice(&loaded_ix);
    buf.extend_from_slice(&price_ix);
    buf.extend_from_slice(&accounts);
    buf.extend_from_slice(&data);
    buf.extend_from_slice(&[0x44; 64]);
    buf
}

fn read_compact(data: &[u8], mut i: usize, max: usize) -> Option<(usize, usize)> {
    let mut n = 0usize;
    let mut shift = 0;
    loop {
        let b = *data.get(i)?;
        i += 1;
        n |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            if n > max {
                return None;
            }
            return Some((n, i));
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = n;
        k
    }

    fn push_compact(buf: &mut Vec<u8>, mut n: usize) {
        loop {
            let mut b = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                b |= 0x80;
            }
            buf.push(b);
            if n == 0 {
                break;
            }
        }
    }

    /// Legacy: signature, keys [fee, pool, program], one instruction on the program with the pool account.
    fn legacy_tx(pool: [u8; 32], program: [u8; 32]) -> Vec<u8> {
        let mut buf = Vec::new();
        push_compact(&mut buf, 1);
        buf.extend_from_slice(&[0x11; 64]);
        buf.extend_from_slice(&[1, 0, 1]);
        push_compact(&mut buf, 3);
        buf.extend_from_slice(&key(1));
        buf.extend_from_slice(&pool);
        buf.extend_from_slice(&program);
        buf.extend_from_slice(&[0xAB; 32]);
        push_compact(&mut buf, 1);
        buf.push(2);
        push_compact(&mut buf, 1);
        buf.push(1);
        push_compact(&mut buf, 0);
        buf
    }

    #[test]
    fn legacy_hit_names_pool_and_program() {
        let pool = key(9);
        let program = venue_pda::PUMP;
        let raw = legacy_tx(pool, program);
        let tx = decode_transaction(&raw).unwrap();
        assert!(!tx.versioned);
        assert_eq!(tx.signature, [0x11; 64]);
        assert_eq!(tx.unresolved_lookups, 0);
        let h = hit(&tx, &[pool], &watched_dex_programs()).unwrap();
        assert_eq!(h.pools, vec![pool]);
        assert_eq!(h.programs, vec![program]);
    }

    #[test]
    fn v0_keeps_static_pool_and_counts_lookups() {
        let pool = key(4);
        let program = venue_pda::DLMM;
        let mut buf = legacy_tx(pool, program);
        // The v0 version sits before the header. In legacy the header starts right after the signature.
        let header_at = 1 + 64;
        buf.insert(header_at, 0x80);
        push_compact(&mut buf, 1);
        buf.extend_from_slice(&key(8));
        push_compact(&mut buf, 1);
        buf.push(0);
        push_compact(&mut buf, 0);
        let tx = decode_transaction(&buf).unwrap();
        assert!(tx.versioned);
        assert_eq!(tx.unresolved_lookups, 1);
        assert!(tx.static_keys.contains(&pool));
        assert!(hit(&tx, &[pool], &[]).is_some());
    }

    #[test]
    fn truncated_and_unknown_version_are_none() {
        assert!(decode_transaction(&[1]).is_none());
        let mut buf = legacy_tx(key(3), venue_pda::CPMM);
        let header_at = 1 + 64;
        buf[header_at] = 0x81;
        // Inserting an unknown version: overwriting the first header byte is not enough,
        // because 0x81 would be read as num_required_signatures only when the bit is set.
        buf.insert(header_at, 0x81);
        assert!(decode_transaction(&buf).is_none());
    }

    #[test]
    fn scan_skips_votes_discards_and_unrelated() {
        let pool = key(7);
        let raw = legacy_tx(pool, venue_pda::CPMM);
        let hits = scan(
            [
                Packet { bytes: &raw, discard: true, simple_vote: false },
                Packet { bytes: &raw, discard: false, simple_vote: true },
                Packet { bytes: &legacy_tx(key(2), key(3)), discard: false, simple_vote: false },
                Packet { bytes: &raw, discard: false, simple_vote: false },
            ],
            &[pool],
            &watched_dex_programs(),
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].pools, vec![pool]);
        assert_eq!(hits[0].programs, vec![venue_pda::CPMM]);
    }

    #[test]
    fn pancake_v1_sqrt_is_the_direction() {
        let pool_id = key(9);
        let pool = engine::Pool {
            id: pool_id,
            token_a: key(1),
            token_b: key(2),
            kind: engine::Kind::Cp { res_a: 1, res_b: 1, fee_bps: 0 },
        };
        let mut data = vec![0u8; 41];
        data[..8].copy_from_slice(&anchor("swap"));
        data[8..16].copy_from_slice(&1_000u64.to_le_bytes());
        data[24..40].copy_from_slice(&RAYDIUM_SQRT_MIN_PLUS_ONE.to_le_bytes());
        data[40] = 1;
        let ix = Instruction { program: venue_pda::PANCAKE, account_indexes: vec![0, 1, 2], data };
        let swap = swap_from_ix(&ix, &[key(4), key(5), pool_id], &[pool]).unwrap();
        assert!(swap.a_to_b);
        assert_eq!(swap.amount_in, 1_000);
    }

    #[test]
    fn manifest_exact_in_reads_the_amount_at_offset_1() {
        let pool_id = key(9);
        let pool = engine::Pool {
            id: pool_id,
            token_a: key(1),
            token_b: key(2),
            kind: engine::Kind::Cp { res_a: 1, res_b: 1, fee_bps: 0 },
        };
        let mut data = vec![0u8; 19];
        data[0] = 0x0d;
        data[1..9].copy_from_slice(&7_000u64.to_le_bytes());
        data[9..17].copy_from_slice(&99_000u64.to_le_bytes());
        data[17] = 1;
        data[18] = 1;
        let ix = Instruction { program: venue_pda::MANIFEST, account_indexes: vec![3], data: data.clone() };
        let swap = swap_from_ix(&ix, &[key(4), key(5), key(6), pool_id], &[pool.clone()]).unwrap();
        assert!(swap.a_to_b);
        assert_eq!(swap.amount_in, 7_000);
        assert_eq!(swap.pool, pool_id);

        data[17] = 0;
        let ix = Instruction { program: venue_pda::MANIFEST, account_indexes: vec![3], data: data.clone() };
        let swap = swap_from_ix(&ix, &[key(4), key(5), key(6), pool_id], &[pool.clone()]).unwrap();
        assert!(!swap.a_to_b);

        data[18] = 0;
        let ix = Instruction { program: venue_pda::MANIFEST, account_indexes: vec![3], data };
        assert!(swap_from_ix(&ix, &[key(4), key(5), key(6), pool_id], &[pool]).is_none());
    }

    #[test]
    fn backrun_price_is_strictly_below_the_victim() {
        assert_eq!(backrun_cu_price(80_000), Some(79_999));
        assert_eq!(backrun_cu_price(1), Some(0));
        assert_eq!(backrun_cu_price(0), None);
        let price = backrun_cu_price(80_000).unwrap();
        assert!(price < 80_000);
    }

    fn cp_pool(pid: u8) -> engine::Pool {
        engine::Pool {
            id: key(pid),
            token_a: key(1),
            token_b: key(2),
            kind: engine::Kind::Cp { res_a: 1_000_000, res_b: 1_000_000, fee_bps: 30 },
        }
    }

    #[test]
    fn cpmm_wire_swap_names_pool_amount_and_direction() {
        let pools = [cp_pool(10), cp_pool(11)];
        let tx = decode_transaction(&cpmm_exact_in_legacy(key(1), key(2), 400_000, 2)).unwrap();
        assert_eq!(tx.cu_price_micro, Some(2));
        assert_eq!(tx.cu_limit, Some(200_000));
        assert_eq!(tx.loaded_accounts_bytes, Some(131_072));
        assert_eq!(tx.n_signatures, 1);
        assert_eq!(tx.n_write_locks, 1);
        assert!(scheduler_priority(&tx).is_some());
        assert_eq!(tx.instructions.len(), 4);
        let swap = &tx.instructions[3];
        assert_eq!(swap.account_indexes[3], 5);
        assert_eq!(swap.account_indexes[10], 10);
        assert_eq!(swap.account_indexes[12], 200);
        let swaps = victim_swaps(&tx, &pools);
        assert_eq!(swaps.len(), 1);
        assert_eq!(swaps[0].pool, key(10));
        assert!(swaps[0].a_to_b);
        assert_eq!(swaps[0].amount_in, 400_000);

        let rev = decode_transaction(&cpmm_exact_in_legacy(key(2), key(1), 400_000, 2)).unwrap();
        let swaps = victim_swaps(&rev, &pools);
        assert_eq!(swaps.len(), 1);
        assert!(!swaps[0].a_to_b);
        assert_eq!(swaps[0].amount_in, 400_000);

        let zero = decode_transaction(&cpmm_exact_in_legacy(key(1), key(2), 0, 2)).unwrap();
        assert!(victim_swaps(&zero, &pools).is_empty());
        let unknown = decode_transaction(&cpmm_exact_in_legacy(key(9), key(8), 400_000, 2)).unwrap();
        assert!(victim_swaps(&unknown, &pools).is_empty());
    }

    #[test]
    fn pump_buy_exact_quote_in_is_quote_to_base() {
        let pools = [cp_pool(10)];
        let buy = decode_transaction(&pump_leg_legacy("buy_exact_quote_in", 250_000, true)).unwrap();
        let swaps = victim_swaps(&buy, &pools);
        assert_eq!(swaps.len(), 1);
        assert_eq!(swaps[0].pool, key(10));
        assert!(!swaps[0].a_to_b);
        assert_eq!(swaps[0].amount_in, 250_000);

        let sell = decode_transaction(&pump_leg_legacy("sell", 80_000, false)).unwrap();
        let swaps = victim_swaps(&sell, &pools);
        assert_eq!(swaps.len(), 1);
        assert!(swaps[0].a_to_b);
        assert_eq!(swaps[0].amount_in, 80_000);

        let exact_out = decode_transaction(&pump_leg_legacy("buy", 250_000, true)).unwrap();
        assert!(victim_swaps(&exact_out, &pools).is_empty());
    }

    #[test]
    fn reads_victim_cu_price_and_orders_after_it() {
        let mut buf = Vec::new();
        push_compact(&mut buf, 1);
        buf.extend_from_slice(&[0x22; 64]);
        buf.extend_from_slice(&[1, 0, 1]);
        push_compact(&mut buf, 2);
        buf.extend_from_slice(&key(1));
        buf.extend_from_slice(&send::tx::COMPUTE_BUDGET);
        buf.extend_from_slice(&[0xCD; 32]);
        push_compact(&mut buf, 1);
        buf.push(1);
        push_compact(&mut buf, 0);
        let price_ix = send::compute_budget::set_cu_price(50_000);
        push_compact(&mut buf, price_ix.len());
        buf.extend_from_slice(&price_ix);
        let tx = decode_transaction(&buf).unwrap();
        let order = backrun_order(&tx).unwrap();
        assert_eq!(order.cu_price_micro, 49_999);
        assert_eq!(order.victim_signature, [0x22; 64]);
    }

    fn ix_data_bytes(tx: &DecodedTx) -> usize {
        tx.instructions.iter().map(|ix| ix.data.len()).sum()
    }

    #[test]
    fn v1_cpmm_uses_the_config_fee_not_compute_budget() {
        let pools = [cp_pool(10), cp_pool(11)];
        let legacy = decode_transaction(&cpmm_exact_in_legacy(key(1), key(2), 400_000, 2)).unwrap();
        let raw = cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(200_000), Some(131_072), None);
        let tx = decode_transaction(&raw).unwrap();
        assert!(tx.v1);
        assert!(!tx.versioned);
        assert_eq!(tx.unresolved_lookups, 0);
        assert_eq!(tx.signature, [0x44; 64]);
        assert_eq!(tx.n_signatures, 1);
        assert_eq!(tx.n_write_locks, legacy.n_write_locks);
        assert_eq!(ix_data_bytes(&tx), ix_data_bytes(&legacy));
        assert_eq!(tx.cu_limit, Some(200_000));
        assert_eq!(tx.loaded_accounts_bytes, Some(131_072));
        assert_eq!(tx.priority_fee_lamports, Some(1));
        assert!(tx.cu_price_micro.is_none(), "SetComputeUnitPrice in v1 is not a price");
        assert_eq!(scheduler_priority(&tx), scheduler_priority(&legacy));
        assert_eq!(victim_swaps(&tx, &pools), victim_swaps(&legacy, &pools));

        let rev = decode_transaction(&cpmm_exact_in_v1(key(2), key(1), 400_000, Some(1), Some(200_000), Some(131_072), None)).unwrap();
        let swaps = victim_swaps(&rev, &pools);
        assert_eq!(swaps.len(), 1);
        assert!(!swaps[0].a_to_b);
        assert_eq!(swaps[0].amount_in, 400_000);

        let zero_fee = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, None, Some(200_000), Some(131_072), None)).unwrap();
        let legacy_free = decode_transaction(&cpmm_exact_in_legacy(key(1), key(2), 400_000, 0)).unwrap();
        assert_eq!(zero_fee.priority_fee_lamports, Some(0));
        assert_eq!(scheduler_priority(&zero_fee), scheduler_priority(&legacy_free));

        let with_heap = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(200_000), Some(131_072), Some(32 * 1024))).unwrap();
        assert_eq!(scheduler_priority(&with_heap), scheduler_priority(&tx));
        assert!(decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(200_000), Some(131_072), Some(256 * 1024))).is_some());
    }

    #[test]
    fn v1_without_a_usable_budget_has_no_priority() {
        let omitted = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), None, None, None)).unwrap();
        assert!(omitted.cu_limit.is_none());
        assert!(omitted.loaded_accounts_bytes.is_none());
        assert!(omitted.cu_price_micro.is_none());
        assert!(scheduler_priority(&omitted).is_none());

        let no_loaded = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(200_000), None, None)).unwrap();
        assert_eq!(no_loaded.cu_limit, Some(200_000));
        assert!(scheduler_priority(&no_loaded).is_none());

        let zero_cu = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(0), Some(131_072), None)).unwrap();
        assert!(scheduler_priority(&zero_cu).is_none());
        let zero_loaded = decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 400_000, Some(1), Some(200_000), Some(0), None)).unwrap();
        assert!(scheduler_priority(&zero_loaded).is_none());
    }

    #[test]
    fn v1_rejects_a_bad_mask_and_trailing_bytes() {
        assert!(decode_transaction(&[0x82]).is_none());
        assert!(decode_transaction(&[0x82, 0x00]).is_none());
        assert!(decode_transaction(&vec![0x81; 4097]).is_none());

        let mut only_low_fee_bit = vec![0x81, 1, 0, 0];
        only_low_fee_bit.extend_from_slice(&1u32.to_le_bytes());
        assert!(decode_transaction(&only_low_fee_bit).is_none());
        let mut unknown_bit = vec![0x81, 1, 0, 0];
        unknown_bit.extend_from_slice(&(1u32 << 5).to_le_bytes());
        assert!(decode_transaction(&unknown_bit).is_none());
        assert!(decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 1, Some(1), Some(200_000), Some(131_072), Some(1_000))).is_none());
        assert!(decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 1, Some(1), Some(200_000), Some(131_072), Some(32 * 1024 + 1))).is_none());
        assert!(decode_transaction(&cpmm_exact_in_v1(key(1), key(2), 1, Some(1), Some(200_000), Some(131_072), Some(257 * 1024))).is_none());

        let mut trailing = cpmm_exact_in_v1(key(1), key(2), 1, Some(1), Some(200_000), Some(131_072), None);
        trailing.push(0);
        assert!(decode_transaction(&trailing).is_none());

        let mut dup = vec![0x81, 1, 0, 0];
        dup.extend_from_slice(&0u32.to_le_bytes());
        dup.extend_from_slice(&[0xAB; 32]);
        dup.push(0);
        dup.push(2);
        dup.extend_from_slice(&key(1));
        dup.extend_from_slice(&key(1));
        dup.extend_from_slice(&[0x11; 64]);
        assert!(decode_transaction(&dup).is_none());
        dup = vec![0x81, 1, 0, 0];
        dup.extend_from_slice(&0u32.to_le_bytes());
        dup.extend_from_slice(&[0xAB; 32]);
        dup.push(0);
        dup.push(2);
        dup.extend_from_slice(&key(1));
        dup.extend_from_slice(&key(2));
        dup.extend_from_slice(&[0x11; 64]);
        let ok = decode_transaction(&dup).unwrap();
        assert!(ok.v1);
        assert_eq!(ok.static_keys.len(), 2);
        assert!(scheduler_priority(&ok).is_none());
    }
}
