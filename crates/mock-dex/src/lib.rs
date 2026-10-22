//! Mock DEX (SBF program) — **only for validating the CPI** of our arbitrage program.
//!
//! Swap instruction: data = `disc(8) + amount_in(8) + min_out(8)`; accounts = `[user_in, user_out,
//! vault_in, vault_out]` (layout as our program passes it for the CPMM venue). Moves balances
//! (u64 @ offset 64) at the rate stored in `vault_out` (u64 bps @ offset 72). The token accounts are
//! owned by this program (it may write them). Ignores authorisation — it is a test stub.

use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey,
};

entrypoint!(process);

fn read_u64(a: &AccountInfo, off: usize) -> u64 {
    let d = a.try_borrow_data().unwrap();
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}
fn write_u64(a: &AccountInfo, off: usize, v: u64) {
    let mut d = a.try_borrow_mut_data().unwrap();
    d[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn process(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 16 || accounts.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount_in = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let (user_in, user_out, vault_in, vault_out) = (&accounts[0], &accounts[1], &accounts[2], &accounts[3]);
    let rate_bps = read_u64(vault_out, 72);
    let out = (amount_in as u128 * rate_bps as u128 / 10_000) as u64;

    write_u64(user_in, 64, read_u64(user_in, 64).checked_sub(amount_in).ok_or(ProgramError::InsufficientFunds)?);
    write_u64(vault_in, 64, read_u64(vault_in, 64) + amount_in);
    write_u64(vault_out, 64, read_u64(vault_out, 64).checked_sub(out).ok_or(ProgramError::InsufficientFunds)?);
    write_u64(user_out, 64, read_u64(user_out, 64) + out);
    Ok(())
}
