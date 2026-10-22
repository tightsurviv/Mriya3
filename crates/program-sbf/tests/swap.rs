//! Tier-2: validation of CPI execution with real token movement. Our program (BPF) does a CPI into the
//! **mock-DEX** (a separate BPF program, `crates/mock-dex`), which moves balances in token accounts
//! at the rate read from the vault account. Round-trip route base→T→base: favourable rate → profit > 0 →
//! `check_profit` lets it through (tx OK, first success path); unfavourable → `Custom(0)`.
//!
//! Both programs are BPF, so the CPI works as on-chain. It proves that the program loop
//! (parse → encode_swap → invoke → read intermediate balance → profit threshold) works on real CPIs.

use solana_program_test::ProgramTest;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

const START: u64 = 1_000_000;
const STATE_SEED: &[u8] = b"state";

fn system_funded() -> Account {
    Account { lamports: 1_000_000_000, data: vec![], owner: system_program::id(), executable: false, rent_epoch: 0 }
}
fn state_account(arb_id: Pubkey, bump: u8, authority: Pubkey) -> Account {
    let mut d = vec![0u8; 34];
    d[0] = 1;
    d[1] = bump;
    d[2..34].copy_from_slice(&authority.to_bytes());
    Account { lamports: 1_000_000_000, data: d, owner: arb_id, executable: false, rent_epoch: 0 }
}

fn token_account(owner: Pubkey, balance: u64, rate: u64) -> Account {
    let mut data = vec![0u8; 80];
    data[64..72].copy_from_slice(&balance.to_le_bytes());
    data[72..80].copy_from_slice(&rate.to_le_bytes());
    Account { lamports: 1_000_000_000, data, owner, executable: false, rent_epoch: 0 }
}
fn plain(len: usize) -> Account {
    Account { lamports: 1_000_000_000, data: vec![0u8; len], owner: Pubkey::default(), executable: false, rent_epoch: 0 }
}

fn route_two_hops(venue: u8, amount_in: u64, min_profit: u64) -> Vec<u8> {
    let hop = arb_program::Hop { venue, direction: 0, n_accounts: 5, token_account_size: 0 };
    arb_program::encode_route(amount_in, min_profit, &[hop, hop], &arb_program::RouteMeta::new(1, 0))
        .expect("2-leg route")
}

/// Round-trip base→T→base through the mock-DEX; returns (tx result, final base balance).
async fn run_round_trip(venue: u8, rate: u64, min_profit: u64) -> (Result<(), solana_program_test::BanksClientError>, u64) {
    let arb_id = Pubkey::new_unique();
    let mock_id = Pubkey::new_unique();
    let bot = Keypair::new();
    let (state_pda, bump) = Pubkey::find_program_address(&[STATE_SEED], &arb_id);
    let mut pt = ProgramTest::new("arb_program_sbf", arb_id, None);
    pt.add_program("mock_dex", mock_id, None); // BPF program from tests/fixtures/mock_dex.so
    pt.add_account(bot.pubkey(), system_funded());
    pt.add_account(state_pda, state_account(arb_id, bump, bot.pubkey())); // authority = bot

    let user_base = Pubkey::new_unique();
    let user_t = Pubkey::new_unique();
    let vault_base = Pubkey::new_unique();
    let vault_t = Pubkey::new_unique();
    pt.add_account(user_base, token_account(mock_id, START, 0));
    pt.add_account(user_t, token_account(mock_id, 0, 0));
    // vault_out hop0 = vault_t (rate base→T); vault_out hop1 = vault_base (rate T→base).
    pt.add_account(vault_t, token_account(mock_id, 100 * START, rate));
    pt.add_account(vault_base, token_account(mock_id, 100 * START, rate));

    // Prefix fillers (the program does not read them for CPMM), they exist only as accounts.
    let fillers: Vec<Pubkey> = (0..6).map(|_| Pubkey::new_unique()).collect();
    for k in &fillers {
        pt.add_account(*k, plain(0));
    }
    let (bridge, wsol, system_a, rent_a, t_mint, tok_prog) =
        (fillers[0], fillers[1], fillers[2], fillers[3], fillers[4], fillers[5]);

    let (banks, _payer, recent) = pt.start().await;

    let w = |k: Pubkey| AccountMeta::new(k, false);
    let r = |k: Pubkey| AccountMeta::new_readonly(k, false);
    let accounts = vec![
        AccountMeta::new(bot.pubkey(), true), // 0 bot (signer/authority)
        r(state_pda),                           // 1
        w(user_base),                           // 2 base ATA (= prefix[2])
        r(bridge),                              // 3
        r(wsol),                                // 4
        r(system_a),                            // 5
        r(rent_a),                              // 6
        w(user_t),                              // 7 intermediate account T (= prefix[7])
        r(t_mint),                              // 8
        r(tok_prog),                            // 9
        r(mock_id),                             // 10 hop0: DEX program
        w(user_base),                           // 11 user_in
        w(user_t),                              // 12 user_out
        w(vault_base),                          // 13 vault_in
        w(vault_t),                             // 14 vault_out (rate)
        r(mock_id),                             // 15 hop1: DEX program
        w(user_t),                              // 16 user_in
        w(user_base),                           // 17 user_out
        w(vault_t),                             // 18 vault_in
        w(vault_base),                          // 19 vault_out (rate)
    ];

    let ix = Instruction { program_id: arb_id, accounts, data: route_two_hops(venue, START, min_profit) };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&bot.pubkey()));
    tx.sign(&[&bot], recent);
    let res = banks.process_transaction(tx).await;
    let final_base = banks.get_account(user_base).await.unwrap().unwrap();
    let final_balance = u64::from_le_bytes(final_base.data[64..72].try_into().unwrap());
    (res, final_balance)
}

const CPMM: u8 = 0x03;
const PUMP: u8 = 0x05;

#[tokio::test]
async fn profitable_round_trip_succeeds() {
    // rate 110% per leg → round-trip 1.21× → profit > 0 → tx OK (first success path).
    let (res, final_balance) = run_round_trip(CPMM, 11_000, 0).await;
    assert!(res.is_ok(), "profitable round-trip should pass, got: {res:?}");
    assert_eq!(final_balance, 1_210_000, "base balance after the round-trip (1e6·1.1·1.1)");
}

#[tokio::test]
async fn unprofitable_round_trip_reverts() {
    // rate 90% per leg → round-trip 0.81× → loss → Custom(0) (revert, capital untouched).
    let (res, _) = run_round_trip(CPMM, 9_000, 0).await;
    let s = format!("{:?}", res.expect_err("losing round-trip must revert"));
    assert!(s.contains("Custom(0)"), "expected Custom(0), got: {s}");
}

#[tokio::test]
async fn profit_below_threshold_reverts() {
    // rate 110% → profit 210_000, threshold 500_000 → below threshold → Custom(profit/1000)=Custom(210).
    let (res, _) = run_round_trip(CPMM, 11_000, 500_000).await;
    let s = format!("{:?}", res.expect_err("profit below threshold must revert"));
    assert!(s.contains("Custom(210)"), "expected Custom(210), got: {s}");
}

#[tokio::test]
async fn pump_sell_round_trip_succeeds() {
    // Venue Pump (0x05), direction 0 = sell (exact-in): the executor must handle it (not None) and pass.
    let (res, final_balance) = run_round_trip(PUMP, 11_000, 0).await;
    assert!(res.is_ok(), "Pump (sell) round-trip should pass, got: {res:?}");
    assert_eq!(final_balance, 1_210_000);
}
