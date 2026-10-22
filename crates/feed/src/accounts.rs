//! Point A: **intermediate route accounts**. The on-chain program does NOT create them (the reference bot creates them
//! per transaction) — the runner must supply them. We do this via **ATA `CreateIdempotent`** in the same
//! transaction: idempotent (no-op when the account exists), persistent and derivable. The instructions
//! are inserted before the program (see `send::tx::arb_instructions_with_setup`).
//!
//! Pure: ATA derivation ([`keys::associated_token_address`]) + instruction building; no network or sending.

use crate::AccountSource;
use pools::Pubkey;
use send::tx::{AccountMeta, Instruction, SYSTEM};

/// Associated token account program (ATA) — `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`.
pub const ATA_PROGRAM: Pubkey = keys::ATA_PROGRAM;
/// Discriminator of the ATA instruction `CreateIdempotent` (1 byte).
pub const CREATE_IDEMPOTENT: u8 = 1;

/// ATA address: `associated_token_address(wallet, token_program, mint)`.
pub fn ata(wallet: &Pubkey, token_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    keys::associated_token_address(wallet, token_program, mint)
}

/// ATA `CreateIdempotent` instruction creating `ata` = ATA(`wallet`, `token_program`, `mint`).
/// Accounts (required order): funder(signer,w) · ata(w) · wallet(ro) · mint(ro) · System(ro) ·
/// token_program(ro). Data: `[1]`. The ATA program itself validates that `ata` is the canonical ATA (so the
/// intermediate account MUST be the bot's ATA — consistent with what we ran in tier-3).
pub fn create_ata_idempotent_ix(
    funder: Pubkey,
    wallet: Pubkey,
    ata: Pubkey,
    mint: Pubkey,
    token_program: Pubkey,
) -> Instruction {
    Instruction {
        program_id: ATA_PROGRAM,
        accounts: vec![
            AccountMeta { pubkey: funder, is_signer: true, is_writable: true },
            AccountMeta { pubkey: ata, is_signer: false, is_writable: true },
            AccountMeta { pubkey: wallet, is_signer: false, is_writable: false },
            AccountMeta { pubkey: mint, is_signer: false, is_writable: false },
            AccountMeta { pubkey: SYSTEM, is_signer: false, is_writable: false },
            AccountMeta { pubkey: token_program, is_signer: false, is_writable: false },
        ],
        data: vec![CREATE_IDEMPOTENT],
    }
}

/// Builds `CreateIdempotent` for each intermediate route triple `[account, mint, token_program]`
/// (format of [`send::route::intermediate_triple`] / [`crate::run::PlannedRoute::intermediates`]).
/// Funder = wallet = `bot`. Idempotent, so always safe to insert.
pub fn intermediate_setup_ixs(bot: Pubkey, intermediates: &[[AccountMeta; 3]]) -> Vec<Instruction> {
    intermediates
        .iter()
        .map(|t| create_ata_idempotent_ix(bot, bot, t[0].pubkey, t[1].pubkey, t[2].pubkey))
        .collect()
}

/// Like [`intermediate_setup_ixs`], but skips accounts already present in the source (optimization with a
/// live feed: we do not add `CreateIdempotent` for an ATA that `src` already sees). For missing ones — it creates.
pub fn intermediate_setup_ixs_missing(
    bot: Pubkey,
    intermediates: &[[AccountMeta; 3]],
    src: &dyn AccountSource,
) -> Vec<Instruction> {
    intermediates
        .iter()
        .filter(|t| src.get_account(&t[0].pubkey).is_none())
        .map(|t| create_ata_idempotent_ix(bot, bot, t[0].pubkey, t[1].pubkey, t[2].pubkey))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MapSource;
    use send::route::{intermediate_triple, TOKEN, TOKEN_2022};

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    #[test]
    fn create_idempotent_layout() {
        let ix = create_ata_idempotent_ix(id(1), id(1), id(50), id(2), TOKEN);
        assert_eq!(ix.program_id, ATA_PROGRAM);
        assert_eq!(ix.data, vec![1], "CreateIdempotent discriminator = 1");
        assert_eq!(ix.accounts.len(), 6);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable, "funder signer+writable");
        assert!(ix.accounts[1].is_writable && !ix.accounts[1].is_signer, "ata writable");
        assert_eq!(ix.accounts[1].pubkey, id(50));
        assert_eq!(ix.accounts[3].pubkey, id(2), "mint");
        assert_eq!(ix.accounts[4].pubkey, SYSTEM);
        assert_eq!(ix.accounts[5].pubkey, TOKEN, "token program");
    }

    #[test]
    fn setup_ixs_one_per_intermediate() {
        // Two intermediate triples (T via SPL Token, U via Token-2022).
        let inter = vec![
            intermediate_triple(id(50), id(2), TOKEN),
            intermediate_triple(id(51), id(3), TOKEN_2022),
        ];
        let ixs = intermediate_setup_ixs(id(1), &inter);
        assert_eq!(ixs.len(), 2);
        // Each creates exactly the account from the triple, with the right mint and program.
        assert_eq!(ixs[0].accounts[1].pubkey, id(50));
        assert_eq!(ixs[0].accounts[3].pubkey, id(2));
        assert_eq!(ixs[0].accounts[5].pubkey, TOKEN);
        assert_eq!(ixs[1].accounts[1].pubkey, id(51));
        assert_eq!(ixs[1].accounts[5].pubkey, TOKEN_2022);
        // Funder = bot in both.
        assert!(ixs.iter().all(|ix| ix.accounts[0].pubkey == id(1)));
    }

    #[test]
    fn missing_skips_present_accounts() {
        let inter = vec![
            intermediate_triple(id(50), id(2), TOKEN),  // present in the source → skip
            intermediate_triple(id(51), id(3), TOKEN),  // missing → create
        ];
        let mut src = MapSource::new();
        src.insert(id(50), vec![0u8; 165]); // ATA T already exists
        let ixs = intermediate_setup_ixs_missing(id(1), &inter, &src);
        assert_eq!(ixs.len(), 1, "only the missing account");
        assert_eq!(ixs[0].accounts[1].pubkey, id(51));
    }

    #[test]
    fn ata_matches_keys_derivation() {
        // ATA computed here = the same as keys::associated_token_address.
        let got = ata(&id(7), &TOKEN, &id(8));
        assert_eq!(got, keys::associated_token_address(&id(7), &TOKEN, &id(8)));
    }
}
