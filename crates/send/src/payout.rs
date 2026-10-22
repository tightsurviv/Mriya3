//! Separate payout script, not part of the arbitrage transaction.
//!
//! Measurement: when the native surplus is more than 10 SOL above the ~50 SOL target, a plain
//! legacy transfer goes to the payout address given by the operator. No nonce, no CU price, one signature, fee 5,000.
//! The WSOL target is 1,349.99 SOL. The unwrap path (144 tx, two signatures, fee 10,000) has no
//! written-out instruction list, so only the atom surplus is here, no transaction.
//! Nothing is signed or sent.

use crate::tx::{compile_legacy_message, AccountMeta, Instruction, Pubkey, SYSTEM};

/// WSOL target: exactly 1,349.99 SOL.
pub const WSOL_TARGET_LAMPORTS: u64 = 1_349_990_000_000;
/// Native target is ~50 SOL. The tilde stays: there is no more precise threshold in lamports.
pub const NATIVE_TARGET_LAMPORTS: u64 = 50_000_000_000;
/// Payout goes out only above 10 SOL. Observed minimum is 10.000003 SOL.
pub const MIN_PAYOUT_LAMPORTS: u64 = 10_000_000_000;
/// Plain transfer fee: one signature, no priority fee.
pub const PLAIN_FEE_LAMPORTS: u64 = 5_000;

/// WSOL surplus above 1,349.99 SOL. Zero when the balance is at or below the target.
pub fn wsol_surplus(wsol_atoms: u64) -> u64 {
    wsol_atoms.saturating_sub(WSOL_TARGET_LAMPORTS)
}

/// Plain transfer amount. `None` when, after setting aside ~50 SOL and the 5,000 fee, ≤ 10 SOL remains.
/// The account balance drops to 50 SOL.
pub fn plain_transfer_lamports(native_lamports: u64) -> Option<u64> {
    let amount = native_lamports.saturating_sub(NATIVE_TARGET_LAMPORTS).saturating_sub(PLAIN_FEE_LAMPORTS);
    if amount > MIN_PAYOUT_LAMPORTS {
        Some(amount)
    } else {
        None
    }
}

/// Unsigned legacy message: System `Transfer` from `from` to `to` (operator's payout address).
/// No ComputeBudget and no nonce. `recent_blockhash` is supplied by the caller.
pub fn plain_transfer_message(from: Pubkey, to: Pubkey, recent_blockhash: Pubkey, lamports: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    let ix = Instruction {
        program_id: SYSTEM,
        accounts: vec![
            AccountMeta { pubkey: from, is_signer: true, is_writable: true },
            AccountMeta { pubkey: to, is_signer: false, is_writable: true },
        ],
        data,
    };
    compile_legacy_message(from, recent_blockhash, &[ix])
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
    fn plain_transfer_keeps_about_fifty_sol() {
        let sol = 1_000_000_000u64;
        assert_eq!(plain_transfer_lamports(60 * sol), None, "exactly 10 SOL after the fee does not go out");
        let native = 70 * sol + PLAIN_FEE_LAMPORTS;
        assert_eq!(plain_transfer_lamports(native), Some(20 * sol));
        assert_eq!(wsol_surplus(WSOL_TARGET_LAMPORTS), 0);
        assert_eq!(wsol_surplus(WSOL_TARGET_LAMPORTS + 7), 7);
        assert_eq!(wsol_surplus(0), 0);
    }

    #[test]
    fn plain_message_is_one_legacy_transfer() {
        let from = pk(3);
        let to = pk(5);
        let hash = pk(9);
        let lamports = 10 * 1_000_000_000 + 3_000;
        let msg = plain_transfer_message(from, to, hash, lamports);
        assert_eq!(msg[0], 1, "one signature, legacy message");
        assert_ne!(msg[0], 0x80);
        assert!(msg.windows(32).any(|w| w == to));
        assert!(msg.windows(32).any(|w| w == from));
        assert!(msg.windows(32).any(|w| w == hash));
        let mut data = [0u8; 12];
        data[0..4].copy_from_slice(&2u32.to_le_bytes());
        data[4..12].copy_from_slice(&lamports.to_le_bytes());
        assert!(msg.windows(12).any(|w| w == data));
        assert!(!msg.windows(4).any(|w| w == [4, 0, 0, 0]), "without advance nonce");
    }
}
