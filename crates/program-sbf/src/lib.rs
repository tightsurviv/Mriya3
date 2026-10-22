//! SBF entrypoint of the arbitrage program — ties the [`arb_program`] core (route parser, account
//! dispatcher, CPI encoders, profit threshold, authorisation) to the Solana runtime (balance reads, `invoke`).
//!
//! Instructions: `0x00` Initialize (creates the state PDA), `0x0a` Execute (route).
//! Execute: authorisation (state PDA: owner==program, `data[0]==1`, `data[2..34]==signer`, otherwise
//! `Custom(6902)`) → base balance BEFORE → per leg: input amount → `encode_swap` (min_out=1) →
//! `invoke` into the DEX → balance AFTER → `check_profit`: profit ≤ threshold ⇒ revert (`Custom`).

use arb_program::{
    check_authority, check_profit_with_price, cpi, executor, hop, parse_header, ArbError, STATE_LEN,
    TAG_EXECUTE, TAG_INITIALIZE, UNAUTHORIZED_CODE,
};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// Seed of the state PDA: `PDA(["state"], program_id)`.
const STATE_SEED: &[u8] = b"state";

fn token_amount(acc: &AccountInfo) -> Result<u64, ProgramError> {
    let data = acc.try_borrow_data()?;
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

fn to_err(e: ArbError) -> ProgramError {
    match e {
        ArbError::NoProfit => ProgramError::Custom(0),
        ArbError::BelowThreshold(x) => ProgramError::Custom(x as u32),
        ArbError::BadRoute => ProgramError::Custom(6901),
        ArbError::Unauthorized => ProgramError::Custom(UNAUTHORIZED_CODE),
    }
}

fn input_token_index(i: u8) -> usize {
    if i == 0 {
        executor::BASE_TOKEN_ACCOUNT_INDEX
    } else {
        7 + 3 * (i as usize - 1)
    }
}

pub fn process_instruction(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    match data.first() {
        Some(&TAG_INITIALIZE) => initialize(program_id, accounts, data),
        Some(&TAG_EXECUTE) => execute(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

/// `Initialize`: creates the state PDA `PDA(["state", bump], program_id)` and writes `[1, bump, authority]`.
/// Data: `[0x00, bump]`. Accounts: `[authority(signer,w), state_pda(w), system_program]`.
fn initialize(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let bump = data[1];
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let authority = &accounts[0];
    let state = &accounts[1];
    let system = &accounts[2];
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Verify that state is the canonical PDA for the given bump.
    let expected = Pubkey::create_program_address(&[STATE_SEED, &[bump]], program_id)
        .map_err(|_| ProgramError::InvalidSeeds)?;
    if expected != *state.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(STATE_LEN);
    let ix = system_instruction::create_account(authority.key, state.key, lamports, STATE_LEN as u64, program_id);
    invoke_signed(&ix, &[authority.clone(), state.clone(), system.clone()], &[&[STATE_SEED, &[bump]]])?;
    let mut d = state.try_borrow_mut_data()?;
    d[0] = 1;
    d[1] = bump;
    d[2..34].copy_from_slice(&authority.key.to_bytes());
    Ok(())
}

fn execute(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // Authorisation: [0] bot signs; [1] state PDA belongs to the program and points at the bot.
    if accounts.len() < 2 || !accounts[0].is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let state = &accounts[1];
    if state.owner != program_id {
        return Err(ProgramError::Custom(UNAUTHORIZED_CODE));
    }
    check_authority(&state.try_borrow_data()?, &accounts[0].key.to_bytes()).map_err(to_err)?;

    let header = parse_header(data).map_err(to_err)?;
    let need = executor::total_accounts(data).map_err(to_err)?;
    if accounts.len() < need {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let base_ta = &accounts[executor::BASE_TOKEN_ACCOUNT_INDEX];
    let before = token_amount(base_ta)?;

    let mut i = 0u8;
    while i < header.n_hops {
        let hp = hop(data, i).map_err(to_err)?;
        let (off, len) = executor::hop_account_range(data, i).map_err(to_err)?;
        let hop_accounts = &accounts[off..off + len];

        let in_idx = input_token_index(i);
        let amount = if i == 0 { header.amount_in } else { token_amount(&accounts[in_idx])? };

        let mut buf = [0u8; 64];
        let n = cpi::encode_swap(hp.venue, hp.direction, amount, 1, &mut buf).ok_or(ProgramError::InvalidArgument)?;

        let mut metas: Vec<AccountMeta> = Vec::with_capacity(len.saturating_sub(1));
        for a in &hop_accounts[1..] {
            metas.push(AccountMeta { pubkey: *a.key, is_signer: a.is_signer, is_writable: a.is_writable });
        }
        let ix = Instruction { program_id: *hop_accounts[0].key, accounts: metas, data: buf[..n].to_vec() };
        invoke(&ix, hop_accounts)?;

        i += 1;
    }

    let after = token_amount(base_ta)?;
    check_profit_with_price(before, after, header.min_profit, header.sol_price).map_err(to_err)?;
    Ok(())
}
