//! Transaction building (construction only, NO signing and NO submission).
//!
//! Assembles the instruction list and compiles a **v0 message**. Accounts the operator supplies in the ALT
//! tables go down to lookups; the first used table is lookup #0. An empty table list gives v0
//! without lookups. Signing and submission stay with the operator.

pub type Pubkey = [u8; 32];

/// ID of the ComputeBudget program ComputeBudget111111111111111111111111111111.
pub const COMPUTE_BUDGET: Pubkey = [3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 187, 197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0];
/// System program ID (11111111111111111111111111111111).
pub const SYSTEM: Pubkey = [0u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub program_id: Pubkey,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

/// compact-u16 (shortvec) length encoding.
fn shortvec(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

struct Acc {
    pubkey: Pubkey,
    signer: bool,
    writable: bool,
}

fn upsert(accs: &mut Vec<Acc>, pk: Pubkey, s: bool, w: bool) {
    if let Some(a) = accs.iter_mut().find(|a| a.pubkey == pk) {
        a.signer |= s;
        a.writable |= w;
    } else {
        accs.push(Acc { pubkey: pk, signer: s, writable: w });
    }
}

/// Compiles a legacy message to bytes to sign. `fee_payer` goes to index 0
/// (the only/first required signature). Account order: signer+writable, signer+readonly,
/// nonsigner+writable, nonsigner+readonly; programs as nonsigner+readonly.
pub fn compile_legacy_message(fee_payer: Pubkey, recent_blockhash: Pubkey, ixs: &[Instruction]) -> Vec<u8> {
    let mut accs: Vec<Acc> = Vec::new();
    upsert(&mut accs, fee_payer, true, true);
    for ix in ixs {
        for m in &ix.accounts {
            upsert(&mut accs, m.pubkey, m.is_signer, m.is_writable);
        }
    }
    for ix in ixs {
        upsert(&mut accs, ix.program_id, false, false);
    }

    // Ordering by category, stable within a category (fee_payer stays at 0).
    let mut ordered: Vec<&Acc> = Vec::new();
    for cat in 0..4 {
        for a in &accs {
            let c = match (a.signer, a.writable) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            if c == cat {
                ordered.push(a);
            }
        }
    }

    let num_sig = ordered.iter().filter(|a| a.signer).count() as u8;
    let num_ro_signed = ordered.iter().filter(|a| a.signer && !a.writable).count() as u8;
    let num_ro_unsigned = ordered.iter().filter(|a| !a.signer && !a.writable).count() as u8;
    let index_of = |pk: Pubkey| ordered.iter().position(|a| a.pubkey == pk).unwrap() as u8;

    let mut out = Vec::new();
    out.push(num_sig);
    out.push(num_ro_signed);
    out.push(num_ro_unsigned);
    shortvec(ordered.len(), &mut out);
    for a in &ordered {
        out.extend_from_slice(&a.pubkey);
    }
    out.extend_from_slice(&recent_blockhash);
    shortvec(ixs.len(), &mut out);
    for ix in ixs {
        out.push(index_of(ix.program_id));
        shortvec(ix.accounts.len(), &mut out);
        for m in &ix.accounts {
            out.push(index_of(m.pubkey));
        }
        shortvec(ix.data.len(), &mut out);
        out.extend_from_slice(&ix.data);
    }
    out
}

/// Like [`arb_instructions`], but with **setup** instructions inserted between SetComputeUnitLimit and
/// the program (e.g. `CreateIdempotent` of the intermediate accounts). Order: SetCULimit → setup → program →
/// SetLoadedAccountsDataSizeLimit → optional tip → SetComputeUnitPrice. A tip between the data
/// limit and the CU price is more common than a tip after the price (424 vs 62). Setup stays before the program,
/// because the intermediate accounts must already exist.
#[allow(clippy::too_many_arguments)]
pub fn arb_instructions_with_setup(
    fee_payer: Pubkey,
    arb_program: Pubkey,
    route_data: Vec<u8>,
    route_accounts: Vec<AccountMeta>,
    cu_limit: u32,
    loaded_accounts_data_bytes: u32,
    cu_price_micro: u64,
    tip: Option<(Pubkey, u64)>,
    setup: &[Instruction],
) -> Vec<Instruction> {
    use crate::compute_budget::{set_cu_limit, set_cu_price, set_loaded_accounts_data_size_limit};
    let mut ixs = Vec::with_capacity(4 + setup.len() + tip.is_some() as usize);
    ixs.push(Instruction { program_id: COMPUTE_BUDGET, accounts: vec![], data: set_cu_limit(cu_limit).to_vec() });
    ixs.extend_from_slice(setup); // intermediate accounts (CreateIdempotent) before the program
    ixs.push(Instruction { program_id: arb_program, accounts: route_accounts, data: route_data });
    ixs.push(Instruction {
        program_id: COMPUTE_BUDGET,
        accounts: vec![],
        data: set_loaded_accounts_data_size_limit(loaded_accounts_data_bytes).to_vec(),
    });
    if let Some((dest, lamports)) = tip {
        // System::transfer(fee_payer -> dest): tag 2 (u32 LE) + lamports (u64 LE).
        let mut d = Vec::with_capacity(12);
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&lamports.to_le_bytes());
        ixs.push(Instruction {
            program_id: SYSTEM,
            accounts: vec![
                AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
                AccountMeta { pubkey: dest, is_signer: false, is_writable: true },
            ],
            data: d,
        });
    }
    ixs.push(Instruction { program_id: COMPUTE_BUDGET, accounts: vec![], data: set_cu_price(cu_price_micro).to_vec() });
    ixs
}

/// Assembles the arbitrage instruction list without the nonce prefix. Shared by
/// [`build_arb_message`] and [`crate::nonce::build_arb_message_with_nonce`].
#[allow(clippy::too_many_arguments)]
pub fn arb_instructions(
    fee_payer: Pubkey,
    arb_program: Pubkey,
    route_data: Vec<u8>,
    route_accounts: Vec<AccountMeta>,
    cu_limit: u32,
    loaded_accounts_data_bytes: u32,
    cu_price_micro: u64,
    tip: Option<(Pubkey, u64)>,
) -> Vec<Instruction> {
    arb_instructions_with_setup(
        fee_payer,
        arb_program,
        route_data,
        route_accounts,
        cu_limit,
        loaded_accounts_data_bytes,
        cu_price_micro,
        tip,
        &[],
    )
}

/// Like [`build_arb_message`], but with setup instructions (intermediate accounts) before the program.
#[allow(clippy::too_many_arguments)]
pub fn build_arb_message_with_setup(
    fee_payer: Pubkey,
    recent_blockhash: Pubkey,
    arb_program: Pubkey,
    route_data: Vec<u8>,
    route_accounts: Vec<AccountMeta>,
    cu_limit: u32,
    loaded_accounts_data_bytes: u32,
    cu_price_micro: u64,
    tip: Option<(Pubkey, u64)>,
    setup: &[Instruction],
    alts: &[AddressLookupTable],
) -> Vec<u8> {
    let ixs = arb_instructions_with_setup(
        fee_payer,
        arb_program,
        route_data,
        route_accounts,
        cu_limit,
        loaded_accounts_data_bytes,
        cu_price_micro,
        tip,
        setup,
    );
    compile_v0_message(fee_payer, recent_blockhash, &ixs, alts)
}

#[allow(clippy::too_many_arguments)]
pub fn build_arb_message(
    fee_payer: Pubkey,
    recent_blockhash: Pubkey,
    arb_program: Pubkey,
    route_data: Vec<u8>,
    route_accounts: Vec<AccountMeta>,
    cu_limit: u32,
    loaded_accounts_data_bytes: u32,
    cu_price_micro: u64,
    tip: Option<(Pubkey, u64)>,
    alts: &[AddressLookupTable],
) -> Vec<u8> {
    build_arb_message_with_setup(
        fee_payer,
        recent_blockhash,
        arb_program,
        route_data,
        route_accounts,
        cu_limit,
        loaded_accounts_data_bytes,
        cu_price_micro,
        tip,
        &[],
        alts,
    )
}

/// ALT table: its address and the list of addresses it stores.
#[derive(Clone, Debug)]
pub struct AddressLookupTable {
    pub address: Pubkey,
    pub addresses: Vec<Pubkey>,
}

/// The ALT account header is 56 B. Addresses start right after it, 32 B each.
/// Layout from a mainnet account and from `solana-program/address-lookup-table` (`LOOKUP_TABLE_META_SIZE`):
/// `u32` variant 1, `u64` deactivation, `u64` last extension, `u8` start index,
/// `u8` authority tag (0 none, 1 present), 32 B key, `u16` padding.
pub const LOOKUP_TABLE_META_SIZE: usize = 56;
pub const LOOKUP_TABLE_MAX_ADDRESSES: usize = 256;

/// Decoded header and addresses. The account owner (`AddressLookupTab1e…`) is checked by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedLookupTable {
    pub deactivation_slot: u64,
    pub last_extended_slot: u64,
    pub last_extended_slot_start_index: u8,
    pub authority: Option<Pubkey>,
    pub addresses: Vec<Pubkey>,
}

/// Reads addresses from the ALT account bytes. It does not know the DiPcRv8 table address and does not insert it.
pub fn decode_lookup_table(data: &[u8]) -> Option<DecodedLookupTable> {
    if data.len() < LOOKUP_TABLE_META_SIZE || (data.len() - LOOKUP_TABLE_META_SIZE) % 32 != 0 {
        return None;
    }
    let n = (data.len() - LOOKUP_TABLE_META_SIZE) / 32;
    if n > LOOKUP_TABLE_MAX_ADDRESSES {
        return None;
    }
    let variant = u32::from_le_bytes(data[0..4].try_into().ok()?);
    if variant != 1 {
        return None;
    }
    let tag = data[21];
    let authority = match tag {
        0 => None,
        1 => {
            let mut key = [0u8; 32];
            key.copy_from_slice(&data[22..54]);
            Some(key)
        }
        _ => return None,
    };
    let mut addresses = Vec::with_capacity(n);
    for chunk in data[LOOKUP_TABLE_META_SIZE..].chunks(32) {
        let mut key = [0u8; 32];
        key.copy_from_slice(chunk);
        addresses.push(key);
    }
    Some(DecodedLookupTable {
        deactivation_slot: u64::from_le_bytes(data[4..12].try_into().ok()?),
        last_extended_slot: u64::from_le_bytes(data[12..20].try_into().ok()?),
        last_extended_slot_start_index: data[20],
        authority,
        addresses,
    })
}

/// Compiles a **v0 message with ALT**. Non-signer, non-program accounts that are found
/// in any of `alts` are loaded via ALT (saves space); the rest go to the static
/// keys. Returns the v0 message bytes to sign. No keys, no submission.
pub fn compile_v0_message(
    fee_payer: Pubkey,
    recent_blockhash: Pubkey,
    ixs: &[Instruction],
    alts: &[AddressLookupTable],
) -> Vec<u8> {
    // 1. Collect accounts with flags + mark programs.
    let mut accs: Vec<Acc> = Vec::new();
    upsert(&mut accs, fee_payer, true, true);
    for ix in ixs {
        for m in &ix.accounts {
            upsert(&mut accs, m.pubkey, m.is_signer, m.is_writable);
        }
    }
    let mut programs: Vec<Pubkey> = Vec::new();
    for ix in ixs {
        upsert(&mut accs, ix.program_id, false, false);
        if !programs.contains(&ix.program_id) {
            programs.push(ix.program_id);
        }
    }

    let in_alt = |pk: Pubkey| -> Option<(usize, usize)> {
        for (ai, alt) in alts.iter().enumerate() {
            if let Some(ix) = alt.addresses.iter().position(|a| *a == pk) {
                if ix <= u8::MAX as usize {
                    return Some((ai, ix));
                }
            }
        }
        None
    };
    let is_static = |a: &Acc| a.signer || programs.contains(&a.pubkey) || in_alt(a.pubkey).is_none();

    // 2. Static keys by category (static only).
    let mut static_keys: Vec<&Acc> = Vec::new();
    for cat in 0..4 {
        for a in &accs {
            if !is_static(a) {
                continue;
            }
            let c = match (a.signer, a.writable) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            if c == cat {
                static_keys.push(a);
            }
        }
    }

    // 3. Lookups in the order of the given tables. All writable addresses go before
    // all read-only ones, as the runtime loads them.
    struct Lookup {
        alt_index: usize,
        writable: Vec<u8>,
        readonly: Vec<u8>,
    }
    let mut lookups: Vec<Lookup> = Vec::new();
    for ai in 0..alts.len() {
        let mut w: Vec<u8> = Vec::new();
        let mut r: Vec<u8> = Vec::new();
        for a in &accs {
            if is_static(a) {
                continue;
            }
            if let Some((aj, idx)) = in_alt(a.pubkey) {
                if aj == ai {
                    if a.writable {
                        w.push(idx as u8);
                    } else {
                        r.push(idx as u8);
                    }
                }
            }
        }
        if w.is_empty() && r.is_empty() {
            continue;
        }
        w.sort_unstable();
        w.dedup();
        r.sort_unstable();
        r.dedup();
        lookups.push(Lookup { alt_index: ai, writable: w, readonly: r });
    }
    let mut writable_loaded: Vec<Pubkey> = Vec::new();
    let mut readonly_loaded: Vec<Pubkey> = Vec::new();
    for lk in &lookups {
        for &idx in &lk.writable {
            writable_loaded.push(alts[lk.alt_index].addresses[idx as usize]);
        }
    }
    for lk in &lookups {
        for &idx in &lk.readonly {
            readonly_loaded.push(alts[lk.alt_index].addresses[idx as usize]);
        }
    }

    // 4. Indexing: static ++ writable_loaded ++ readonly_loaded.
    let s = static_keys.len();
    let wl = writable_loaded.len();
    let index_of = |pk: Pubkey| -> u8 {
        if let Some(p) = static_keys.iter().position(|a| a.pubkey == pk) {
            p as u8
        } else if let Some(p) = writable_loaded.iter().position(|x| *x == pk) {
            (s + p) as u8
        } else {
            let p = readonly_loaded.iter().position(|x| *x == pk).unwrap();
            (s + wl + p) as u8
        }
    };

    // 5. Header (counts only static keys).
    let num_sig = static_keys.iter().filter(|a| a.signer).count() as u8;
    let num_ro_signed = static_keys.iter().filter(|a| a.signer && !a.writable).count() as u8;
    let num_ro_unsigned = static_keys.iter().filter(|a| !a.signer && !a.writable).count() as u8;

    // 6. v0 serialization.
    let mut out = Vec::new();
    out.push(0x80); // version 0 prefix
    out.push(num_sig);
    out.push(num_ro_signed);
    out.push(num_ro_unsigned);
    shortvec(static_keys.len(), &mut out);
    for a in &static_keys {
        out.extend_from_slice(&a.pubkey);
    }
    out.extend_from_slice(&recent_blockhash);
    shortvec(ixs.len(), &mut out);
    for ix in ixs {
        out.push(index_of(ix.program_id));
        shortvec(ix.accounts.len(), &mut out);
        for m in &ix.accounts {
            out.push(index_of(m.pubkey));
        }
        shortvec(ix.data.len(), &mut out);
        out.extend_from_slice(&ix.data);
    }
    shortvec(lookups.len(), &mut out);
    for lk in &lookups {
        out.extend_from_slice(&alts[lk.alt_index].address);
        shortvec(lk.writable.len(), &mut out);
        out.extend_from_slice(&lk.writable);
        shortvec(lk.readonly.len(), &mut out);
        out.extend_from_slice(&lk.readonly);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(n: u8) -> Pubkey {
        let mut k = [0u8; 32];
        k[0] = n;
        k
    }

    #[test]
    fn shortvec_encoding() {
        let mut v = Vec::new();
        shortvec(3, &mut v);
        assert_eq!(v, vec![3]);
        let mut v = Vec::new();
        shortvec(300, &mut v);
        assert_eq!(v, vec![0xAC, 0x02]); // 300 = 0b100101100
    }

    #[test]
    fn compiles_simple_message() {
        let fee_payer = pk(1);
        let program = pk(200);
        let readonly = pk(50);
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
                AccountMeta { pubkey: readonly, is_signer: false, is_writable: false },
            ],
            data: vec![9, 9, 9],
        };
        let msg = compile_legacy_message(fee_payer, pk(255), &[ix]);
        // header: 1 signature, 0 readonly-signed, 2 readonly-unsigned (readonly + program)
        assert_eq!(&msg[0..3], &[1, 0, 2]);
        // shortvec(3) accounts, then 3*32 bytes
        assert_eq!(msg[3], 3);
        // fee_payer at index 0
        assert_eq!(&msg[4..36], &fee_payer);
    }

    #[test]
    fn lookup_table_addresses_start_after_56_bytes() {
        let mut data = vec![0u8; 56 + 64];
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        data[12..20].copy_from_slice(&40u64.to_le_bytes());
        data[20] = 7;
        data[21] = 1;
        data[22..54].copy_from_slice(&pk(4));
        data[56..88].copy_from_slice(&pk(8));
        data[88..120].copy_from_slice(&pk(9));
        let decoded = decode_lookup_table(&data).expect("table");
        assert_eq!(decoded.deactivation_slot, u64::MAX);
        assert_eq!(decoded.last_extended_slot, 40);
        assert_eq!(decoded.last_extended_slot_start_index, 7);
        assert_eq!(decoded.authority, Some(pk(4)));
        assert_eq!(decoded.addresses, vec![pk(8), pk(9)]);

        data[21] = 0;
        assert_eq!(decode_lookup_table(&data).unwrap().authority, None);
        assert!(decode_lookup_table(&[0u8; 56]).is_none(), "variant 0");
        assert!(decode_lookup_table(&[1u8; 57]).is_none(), "tail not divisible by 32");
        assert!(decode_lookup_table(&vec![1u8; 56 + 32 * 257]).is_none(), "over 256 addresses");
    }

    #[test]
    fn compiles_v0_with_alt() {
        let fee_payer = pk(1);
        let program = pk(100);
        let w = pk(60); // writable, in ALT
        let r = pk(61); // readonly, in ALT
        let alt = AddressLookupTable { address: pk(200), addresses: vec![pk(61), pk(60)] }; // r@0, w@1
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
                AccountMeta { pubkey: w, is_signer: false, is_writable: true },
                AccountMeta { pubkey: r, is_signer: false, is_writable: false },
            ],
            data: vec![1, 2, 3],
        };
        let msg = compile_v0_message(fee_payer, pk(255), &[ix], &[alt]);
        assert_eq!(msg[0], 0x80, "v0 version prefix");
        // static = fee_payer + program (w and r went to ALT) → 2 static keys
        // header: 1 signature, 0 ro-signed, 1 ro-unsigned (program)
        assert_eq!(&msg[1..4], &[1, 0, 1]);
        assert_eq!(msg[4], 2, "2 static keys");
        // at the end 1 lookup: ALT address + writable_indexes [1] + readonly_indexes [0]
        let tail = &msg[msg.len() - (32 + 1 + 1 + 1 + 1)..];
        assert_eq!(&tail[0..32], &pk(200)); // ALT address
        assert_eq!(tail[32], 1); // 1 writable index
        assert_eq!(tail[33], 1); // writable idx = 1 (w@1)
        assert_eq!(tail[34], 1); // 1 readonly index
        assert_eq!(tail[35], 0); // readonly idx = 0 (r@0)
    }

    #[test]
    fn builds_full_arb_message() {
        let fee_payer = pk(1);
        let arb = pk(100);
        let route_accounts = vec![
            AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
            AccountMeta { pubkey: pk(2), is_signer: false, is_writable: true }, // state PDA
            AccountMeta { pubkey: pk(3), is_signer: false, is_writable: true }, // pool
        ];
        let msg = build_arb_message(
            fee_payer,
            pk(255),
            arb,
            vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0], // sample route data
            route_accounts,
            127_000,
            131_072, // limit on the size of loaded account data
            61_825,
            Some((pk(240), 1_921_177)),
            &[],
        );
        assert_eq!(msg[0], 0x80, "v0 message");
        assert_eq!(msg[1], 1, "one signature");
        assert!(msg.len() > 100);
        let loaded = crate::compute_budget::set_loaded_accounts_data_size_limit(131_072);
        let price = crate::compute_budget::set_cu_price(61_825);
        let mut tip = Vec::new();
        tip.extend_from_slice(&2u32.to_le_bytes());
        tip.extend_from_slice(&1_921_177u64.to_le_bytes());
        let loaded_at = msg.windows(loaded.len()).position(|w| w == loaded).unwrap();
        let tip_at = msg.windows(tip.len()).position(|w| w == tip).unwrap();
        let price_at = msg.windows(price.len()).position(|w| w == price).unwrap();
        assert!(loaded_at < tip_at && tip_at < price_at, "tip sits between the data limit and the CU price");
    }

    #[test]
    fn v0_lookups_follow_table_order_and_writable_block() {
        let fee_payer = pk(1);
        let program = pk(100);
        let w0 = pk(60);
        let w1 = pk(61);
        let r1 = pk(62);
        let alt0 = AddressLookupTable { address: pk(200), addresses: vec![pk(9), pk(8), pk(7), w0] };
        let alt1 = AddressLookupTable { address: pk(201), addresses: vec![pk(5), r1, pk(4), pk(3), w1] };
        // w1 is in the second table, and in the instruction it appears before w0. Lookup #0 stays alt0.
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
                AccountMeta { pubkey: w1, is_signer: false, is_writable: true },
                AccountMeta { pubkey: r1, is_signer: false, is_writable: false },
                AccountMeta { pubkey: w0, is_signer: false, is_writable: true },
            ],
            data: vec![1, 2, 3],
        };
        let msg = compile_v0_message(fee_payer, pk(255), &[ix], &[alt0, alt1]);
        let a0 = msg.windows(32).position(|w| w == pk(200)).unwrap();
        let a1 = msg.windows(32).position(|w| w == pk(201)).unwrap();
        assert!(a0 < a1, "the first given table is lookup #0");
        assert_eq!(&msg[a0 + 32..a0 + 35], &[1, 3, 0], "alt0: write idx 3, no read");
        assert_eq!(&msg[a1 + 32..a1 + 36], &[1, 4, 1, 1], "alt1: write idx 4, read idx 1");
        assert!(!msg.windows(32).any(|w| w == w0 || w == w1 || w == r1), "ALT addresses are not static");
    }
}
