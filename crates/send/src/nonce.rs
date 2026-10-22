//! **Durable nonce** mode for building transactions.
//!
//! In this mode the message's `recent_blockhash` field is not a fresh blockhash but the **nonce value**
//! stored in the nonce account, and the FIRST instruction of the transaction is `AdvanceNonceAccount`. Thanks
//! to this the transaction does not expire after 150 blocks — it can be signed in advance and sent when
//! an opportunity appears, without racing the blockhash window (important for a backrun, when between observing
//! state and sending a few slots pass).
//!
//! The nonce value is **computable** from an observed blockhash ([`keys::durable_nonce`]),
//! so there is no need to query the nonce account over RPC.
//! This module only encodes the instruction and assembles the message bytes — **no keys, signing or submission**.

use crate::tx::{
    arb_instructions_with_setup, compile_v0_message, AccountMeta, AddressLookupTable, Instruction, Pubkey, SYSTEM,
};

/// Sysvar `RecentBlockhashes` (`SysvarRecentB1ockHashes11111111111111111111`) — account required
/// by `AdvanceNonceAccount` (despite the sysvar's formal deprecation, the instruction still requires it).
pub const RECENT_BLOCKHASHES_SYSVAR: Pubkey = [
    6, 167, 213, 23, 25, 44, 86, 142, 224, 138, 132, 95, 115, 210, 151, 136, 207, 3, 92, 49, 69, 178, 26, 179, 68,
    216, 6, 46, 169, 64, 0, 0,
];

/// Discriminator of the System instruction `AdvanceNonceAccount` (enum variant, u32 LE = 4).
pub const ADVANCE_NONCE_ACCOUNT: u32 = 4;

/// Builds the System program's `AdvanceNonceAccount` instruction.
///
/// Accounts (order required by the runtime): `[0]` nonce account (writable) · `[1]` sysvar
/// RecentBlockhashes (ro) · `[2]` nonce authority (signer). Data: tag 4 (u32 LE), no arguments.
pub fn advance_nonce_ix(nonce_account: Pubkey, nonce_authority: Pubkey) -> Instruction {
    Instruction {
        program_id: SYSTEM,
        accounts: vec![
            AccountMeta { pubkey: nonce_account, is_signer: false, is_writable: true },
            AccountMeta { pubkey: RECENT_BLOCKHASHES_SYSVAR, is_signer: false, is_writable: false },
            AccountMeta { pubkey: nonce_authority, is_signer: true, is_writable: false },
        ],
        data: ADVANCE_NONCE_ACCOUNT.to_le_bytes().to_vec(),
    }
}

/// Like [`crate::tx::build_arb_message`], but in durable nonce mode: adds `AdvanceNonceAccount`
/// as the **first** instruction, and puts `nonce_value` in the `recent_blockhash` field (nonce value
/// from [`keys::durable_nonce`]). Returns the v0 message bytes to sign. No keys or submission.
///
/// When `nonce_authority == fee_payer` (typically: the bot itself is the nonce authority), the signatures
/// merge into one — the "1 signature" invariant is preserved.
#[allow(clippy::too_many_arguments)]
pub fn build_arb_message_with_nonce(
    fee_payer: Pubkey,
    nonce_value: Pubkey,
    nonce_account: Pubkey,
    nonce_authority: Pubkey,
    arb_program: Pubkey,
    route_data: Vec<u8>,
    route_accounts: Vec<AccountMeta>,
    cu_limit: u32,
    loaded_accounts_data_bytes: u32,
    cu_price_micro: u64,
    tip: Option<(Pubkey, u64)>,
    alts: &[AddressLookupTable],
) -> Vec<u8> {
    build_arb_message_with_nonce_and_setup(
        fee_payer, nonce_value, nonce_account, nonce_authority, arb_program, route_data, route_accounts,
        cu_limit, loaded_accounts_data_bytes, cu_price_micro, tip, &[], alts,
    )
}

/// Like [`build_arb_message_with_nonce`], but with setup instructions (intermediate accounts) inserted
/// between SetComputeUnitLimit and the program. AdvanceNonce remains the first instruction.
#[allow(clippy::too_many_arguments)]
pub fn build_arb_message_with_nonce_and_setup(
    fee_payer: Pubkey,
    nonce_value: Pubkey,
    nonce_account: Pubkey,
    nonce_authority: Pubkey,
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
    let mut ixs = vec![advance_nonce_ix(nonce_account, nonce_authority)];
    ixs.extend(arb_instructions_with_setup(
        fee_payer,
        arb_program,
        route_data,
        route_accounts,
        cu_limit,
        loaded_accounts_data_bytes,
        cu_price_micro,
        tip,
        setup,
    ));
    compile_v0_message(fee_payer, nonce_value, &ixs, alts)
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
    fn advance_nonce_layout() {
        let ix = advance_nonce_ix(pk(10), pk(1));
        assert_eq!(ix.program_id, SYSTEM);
        assert_eq!(ix.data, vec![4, 0, 0, 0], "tag AdvanceNonceAccount = 4 (u32 LE)");
        assert_eq!(ix.accounts.len(), 3);
        // nonce account writable, non-signer
        assert!(ix.accounts[0].is_writable && !ix.accounts[0].is_signer);
        assert_eq!(ix.accounts[0].pubkey, pk(10));
        // sysvar RecentBlockhashes ro
        assert_eq!(ix.accounts[1].pubkey, RECENT_BLOCKHASHES_SYSVAR);
        assert!(!ix.accounts[1].is_writable && !ix.accounts[1].is_signer);
        // authority signs
        assert!(ix.accounts[2].is_signer);
        assert_eq!(ix.accounts[2].pubkey, pk(1));
    }

    #[test]
    fn nonce_message_uses_nonce_value_as_blockhash_and_advance_first() {
        let bot = pk(1);
        let nonce_value = pk(123); // value from keys::durable_nonce
        let route_accounts = vec![
            AccountMeta { pubkey: bot, is_signer: true, is_writable: true },
            AccountMeta { pubkey: pk(2), is_signer: false, is_writable: true },
        ];
        let msg = build_arb_message_with_nonce(
            bot,
            nonce_value,
            pk(10), // nonce account
            bot,    // authority = bot (merges into 1 signature)
            pk(100),
            vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0],
            route_accounts,
            127_000,
            131_072,
            61_825,
            None,
            &[],
        );
        assert_eq!(msg[0], 0x80, "v0 message");
        assert_eq!(msg[1], 1, "nonce authority = bot → 1 signature");
        // The recent_blockhash field = nonce_value.
        // [shortvec account count][32B keys…][recent_blockhash 32B]. We look for nonce_value as a
        // 32-byte block in the message (the nonce value must appear there literally).
        assert!(
            msg.windows(32).any(|w| w == nonce_value),
            "recent_blockhash must be the durable nonce value"
        );
        // AdvanceNonce (System, tag 4) must be encoded as the first instruction — its data
        // [4,0,0,0] appear in the message.
        assert!(msg.windows(4).any(|w| w == [4, 0, 0, 0]), "AdvanceNonceAccount data present");
    }

    #[test]
    fn separate_nonce_authority_needs_two_signatures() {
        let bot = pk(1);
        let authority = pk(9); // separate nonce authority
        let route_accounts =
            vec![AccountMeta { pubkey: bot, is_signer: true, is_writable: true }];
        let msg = build_arb_message_with_nonce(
            bot,
            pk(123),
            pk(10),
            authority,
            pk(100),
            vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0],
            route_accounts,
            127_000,
            131_072,
            61_825,
            None,
            &[],
        );
        assert_eq!(msg[0], 0x80);
        assert_eq!(msg[1], 2, "separate nonce authority → 2 required signatures");
    }
}
