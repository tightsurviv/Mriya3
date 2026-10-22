//! Validation of execution in the BPF machine (`solana-program-test`): loads the compiled `.so` and
//! runs the entrypoint. Tier-1 (no DEX pools): authorisation (state PDA) + revert when there is no profit.

use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};
use solana_program_test::ProgramTest;

const STATE_SEED: &[u8] = b"state";

/// Route `0x0a` with `n = 0`. The parser rejects a leg count outside `2..=6` (`BadRoute` → `Custom(6901)`).
fn route_no_hops(amount_in: u64, min_profit: u64) -> Vec<u8> {
    let mut d = vec![0u8; 62];
    d[0] = 0x0a;
    d[1..9].copy_from_slice(&amount_in.to_le_bytes());
    d[9..17].copy_from_slice(&min_profit.to_le_bytes());
    d[25] = 0;
    d
}

fn plain(data_len: usize) -> Account {
    Account { lamports: 1_000_000_000, data: vec![0u8; data_len], owner: Pubkey::default(), executable: false, rent_epoch: 0 }
}
fn system_funded() -> Account {
    Account { lamports: 1_000_000_000, data: vec![], owner: system_program::id(), executable: false, rent_epoch: 0 }
}
/// State account: `[1, bump, authority]`, owner = program.
fn state_account(arb_id: Pubkey, bump: u8, authority: Pubkey) -> Account {
    let mut d = vec![0u8; 34];
    d[0] = 1;
    d[1] = bump;
    d[2..34].copy_from_slice(&authority.to_bytes());
    Account { lamports: 1_000_000_000, data: d, owner: arb_id, executable: false, rent_epoch: 0 }
}

#[tokio::test]
async fn zero_hop_route_is_rejected() {
    let arb_id = Pubkey::new_unique();
    let bot = Keypair::new();
    let (state_pda, bump) = Pubkey::find_program_address(&[STATE_SEED], &arb_id);
    let base_ta = Pubkey::new_unique();
    let bridge = Pubkey::new_unique();

    let mut pt = ProgramTest::new("arb_program_sbf", arb_id, None);
    pt.add_account(bot.pubkey(), system_funded());
    pt.add_account(state_pda, state_account(arb_id, bump, bot.pubkey())); // authority = bot
    pt.add_account(base_ta, plain(80)); // base balance = 0
    pt.add_account(bridge, plain(0));
    let (banks, _payer, recent) = pt.start().await;

    let ix = Instruction {
        program_id: arb_id,
        accounts: vec![
            AccountMeta::new(bot.pubkey(), true),        // 0 bot = signer/authority
            AccountMeta::new_readonly(state_pda, false), // 1 state PDA
            AccountMeta::new(base_ta, false),            // 2 base ATA
            AccountMeta::new_readonly(bridge, false),    // 3
        ],
        data: route_no_hops(1_000_000, 0),
    };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&bot.pubkey()));
    tx.sign(&[&bot], recent);

    let err = banks.process_transaction(tx).await.expect_err("0-leg route must revert");
    let s = format!("{err:?}");
    assert!(s.contains("Custom(6901)"), "expected Custom(6901), got: {s}");
}

#[tokio::test]
async fn initialize_creates_state_pda() {
    let arb_id = Pubkey::new_unique();
    let bot = Keypair::new();
    let (state_pda, bump) = Pubkey::find_program_address(&[STATE_SEED], &arb_id);

    let mut pt = ProgramTest::new("arb_program_sbf", arb_id, None);
    pt.add_account(bot.pubkey(), system_funded());
    let (banks, _payer, recent) = pt.start().await;

    // Initialize: data [0x00, bump], accounts [authority(signer,w), state_pda(w), system_program].
    let ix = Instruction {
        program_id: arb_id,
        accounts: vec![
            AccountMeta::new(bot.pubkey(), true),
            AccountMeta::new(state_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: vec![0x00, bump],
    };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&bot.pubkey()));
    tx.sign(&[&bot], recent);
    banks.process_transaction(tx).await.expect("Initialize should create the state PDA");

    let acc = banks.get_account(state_pda).await.unwrap().expect("state PDA exists");
    assert_eq!(acc.owner, arb_id, "state PDA belongs to the program");
    assert_eq!(acc.data[0], 1, "initialised flag");
    assert_eq!(acc.data[1], bump);
    assert_eq!(&acc.data[2..34], &bot.pubkey().to_bytes(), "authority = bot");
}

#[tokio::test]
async fn program_rejects_missing_signer() {
    let arb_id = Pubkey::new_unique();
    let bot = Keypair::new();
    let (state_pda, bump) = Pubkey::find_program_address(&[STATE_SEED], &arb_id);
    let base_ta = Pubkey::new_unique();
    let bridge = Pubkey::new_unique();
    let not_signer = Pubkey::new_unique();

    let mut pt = ProgramTest::new("arb_program_sbf", arb_id, None);
    pt.add_account(bot.pubkey(), system_funded());
    pt.add_account(state_pda, state_account(arb_id, bump, bot.pubkey()));
    pt.add_account(base_ta, plain(80));
    pt.add_account(bridge, plain(0));
    let (banks, _payer, recent) = pt.start().await;

    // Account [0] is NOT a signer → MissingRequiredSignature (the bot pays, but slot [0] is a different key).
    let ix = Instruction {
        program_id: arb_id,
        accounts: vec![
            AccountMeta::new_readonly(not_signer, false),
            AccountMeta::new_readonly(state_pda, false),
            AccountMeta::new(base_ta, false),
            AccountMeta::new_readonly(bridge, false),
        ],
        data: route_no_hops(1, 0),
    };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&bot.pubkey()));
    tx.sign(&[&bot], recent);
    let err = banks.process_transaction(tx).await.expect_err("missing signature must revert");
    assert!(format!("{err:?}").contains("MissingRequiredSignature"));
}

#[tokio::test]
async fn program_rejects_wrong_authority() {
    let arb_id = Pubkey::new_unique();
    let bot = Keypair::new();
    let (state_pda, bump) = Pubkey::find_program_address(&[STATE_SEED], &arb_id);
    let base_ta = Pubkey::new_unique();
    let bridge = Pubkey::new_unique();

    let mut pt = ProgramTest::new("arb_program_sbf", arb_id, None);
    pt.add_account(bot.pubkey(), system_funded());
    // state authority = someone OTHER than the bot → authorisation must reject.
    pt.add_account(state_pda, state_account(arb_id, bump, Pubkey::new_unique()));
    pt.add_account(base_ta, plain(80));
    pt.add_account(bridge, plain(0));
    let (banks, _payer, recent) = pt.start().await;

    let ix = Instruction {
        program_id: arb_id,
        accounts: vec![
            AccountMeta::new(bot.pubkey(), true), // signs, but is not the authority from state
            AccountMeta::new_readonly(state_pda, false),
            AccountMeta::new(base_ta, false),
            AccountMeta::new_readonly(bridge, false),
        ],
        data: route_no_hops(1, 0),
    };
    let mut tx = Transaction::new_with_payer(&[ix], Some(&bot.pubkey()));
    tx.sign(&[&bot], recent);
    let err = banks.process_transaction(tx).await.expect_err("wrong authorisation must revert");
    let s = format!("{err:?}");
    assert!(s.contains("Custom(6902)"), "expected Custom(6902), got: {s}");
}
