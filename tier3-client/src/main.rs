//! Tier-3: runs our swap through a CLONED real pool on a local validator.
//! Arguments: <program_id> [venue]  (venue = cpmm | dammv2, default cpmm).
//! Initialize → wrap WSOL → token ATA → 1-leg route WSOL→token through our program.
//! Simulates the route and prints the logs: if the DEX is invoked (invoke [2]) and our program returns
//! Custom(0) (no profit on 1 leg) → the account layout + CPI bytes match the live exchange. Additionally
//! we pass the same tx through our submit layer (send::submit + feed::submit) — without sending.

use std::str::FromStr;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program,
    transaction::Transaction,
};

// --- Raydium CPMM (venue 0x03) ---
const POOL: &str = "7wTChQDj3KG4kHXYjWrPw5bWUkKzNpJySU5CABEkHogv";
const AMMCFG: &str = "D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2";
const OBS: &str = "AEUT5QncT8Gpg41qLpsXup3MY39LNrWwZYFXNSG5iq2V";
const VAULT_TOK: &str = "BjxbTebtrZu44CVfZ2GzDLKZrmyitFTkny1WqeWmmVxH";
const VAULT_WSOL: &str = "GycqBfycFFRcURh3aWVCpzcFSicsk23omeZPtdnguGvd";
const MINT_TOK: &str = "BzMWbt7ko3P8c457gzxuBCCt6q73sJyG98nQNeTfcCom";
const CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
const CPMM_AUTHORITY: &str = "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL";

// --- Meteora DAMM v2 (venue 0x08) — real WSOL pool from Mriya's tx ---
const DAMM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
const DAMM_POOL_AUTH: &str = "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC";
const DAMM_EVENT_AUTH: &str = "3rmHSu74h1ZcmAisVcWerTCiRDQbUrBKmcwptYGjHfet";
const DAMM_POOL: &str = "GtPrGitz1jtUpHEsm734PYmiURQxYVC4hUWSEfVxMhUb";
const DAMM_VAULT_A: &str = "3Tc15ckbDfxw1vXV2XEQoAzaovBmqLohbZvVWAwLNTSY";
const DAMM_VAULT_B: &str = "CLR29ThHgQyek3T5BNM5u3zihPc76xW8HHGXpqLqZ1Ht";
const DAMM_MINT_A: &str = "At541hRZhK9LWKj9w2dqN2wCdpFyZgRa1Un2yG7xa43m"; // Token-2022 (memecoin)

const WSOL: &str = "So11111111111111111111111111111111111111112";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const ATA_PROG: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

fn pk(s: &str) -> Pubkey {
    Pubkey::from_str(s).unwrap()
}

fn ata(wallet: &Pubkey, token_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[wallet.as_ref(), token_program.as_ref(), mint.as_ref()], &pk(ATA_PROG)).0
}

/// ATA CreateIdempotent (data = [1]).
fn create_ata(funder: &Pubkey, wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Instruction {
    let a = ata(wallet, token_program, mint);
    Instruction {
        program_id: pk(ATA_PROG),
        accounts: vec![
            AccountMeta::new(*funder, true),
            AccountMeta::new(a, false),
            AccountMeta::new_readonly(*wallet, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![1],
    }
}

/// SPL Token SyncNative (data = [17]).
fn sync_native(account: &Pubkey) -> Instruction {
    Instruction { program_id: pk(TOKEN), accounts: vec![AccountMeta::new(*account, false)], data: vec![17] }
}

fn send(rpc: &RpcClient, ixs: &[Instruction], payer: &Keypair, label: &str) {
    let bh = rpc.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], bh);
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(s) => println!("[OK] {label}: {s}"),
        Err(e) => {
            println!("[ERR] {label}: {e:?}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let program_id = pk(&std::env::args().nth(1).expect("provide program_id"));
    let venue = std::env::args().nth(2).unwrap_or_else(|| "cpmm".into());
    let rpc = RpcClient::new_with_commitment("http://127.0.0.1:8899".to_string(), CommitmentConfig::confirmed());
    let bot = Keypair::new();
    println!("venue: {venue} | bot: {}", bot.pubkey());

    let sig = rpc.request_airdrop(&bot.pubkey(), 5 * LAMPORTS_PER_SOL).unwrap();
    loop {
        if rpc.confirm_transaction(&sig).unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    println!("airdrop ok, balance {} lamports", rpc.get_balance(&bot.pubkey()).unwrap());

    // 1. Initialize state PDA.
    let (state_pda, bump) = Pubkey::find_program_address(&[b"state"], &program_id);
    let init = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(bot.pubkey(), true),
            AccountMeta::new(state_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: vec![0u8, bump],
    };
    send(&rpc, &[init], &bot, "Initialize state PDA");

    // 2. WSOL ATA + wrap 0.02 SOL.
    let wsol = pk(WSOL);
    let token = pk(TOKEN);
    let wsol_ata = ata(&bot.pubkey(), &token, &wsol);
    send(
        &rpc,
        &[
            create_ata(&bot.pubkey(), &bot.pubkey(), &wsol, &token),
            system_instruction::transfer(&bot.pubkey(), &wsol_ata, 2 * LAMPORTS_PER_SOL / 100),
            sync_native(&wsol_ata),
        ],
        &bot,
        "wrap WSOL",
    );

    // 3. Output token ATA (venue-specific; both tokens are Token-2022). Point A: CreateIdempotent.
    let t22 = pk(TOKEN2022);
    let out_mint = match venue.as_str() {
        "dammv2" => pk(DAMM_MINT_A),
        _ => pk(MINT_TOK),
    };
    let out_ata = ata(&bot.pubkey(), &t22, &out_mint);
    send(&rpc, &[create_ata(&bot.pubkey(), &bot.pubkey(), &out_mint, &t22)], &bot, "create token ATA");

    // 4. 1-leg route WSOL→token through our program. amount_in = 0.01 WSOL, threshold 0.
    let amount_in: u64 = LAMPORTS_PER_SOL / 100;
    let w = AccountMeta::new;
    let r = AccountMeta::new_readonly;
    let rent = solana_sdk::sysvar::rent::id();
    let prefix = vec![
        AccountMeta::new(bot.pubkey(), true), // 0 bot (signer)
        r(state_pda, false),                  // 1 state PDA
        w(wsol_ata, false),                   // 2 base ATA (WSOL)
        r(wsol, false),                       // 3 native-bridge placeholder
        r(wsol, false),                       // 4 WSOL mint
        r(system_program::id(), false),       // 5 System
        r(rent, false),                       // 6 Rent
    ];

    let (venue_code, hop): (u8, Vec<AccountMeta>) = match venue.as_str() {
        "dammv2" => (
            0x08,
            vec![
                r(pk(DAMM), false),          // 0 DEX program
                r(pk(DAMM_POOL_AUTH), false), // 1 pool authority
                w(pk(DAMM_POOL), false),     // 2 pool
                w(wsol_ata, false),          // 3 input_token_account (WSOL)
                w(out_ata, false),           // 4 output_token_account (token)
                w(pk(DAMM_VAULT_A), false),  // 5 token_a_vault
                w(pk(DAMM_VAULT_B), false),  // 6 token_b_vault
                r(pk(DAMM_MINT_A), false),   // 7 token_a_mint
                r(wsol, false),              // 8 token_b_mint (WSOL)
                r(bot.pubkey(), true),       // 9 payer (signer, ro)
                r(t22, false),               // 10 token_a_program (Token-2022)
                r(token, false),             // 11 token_b_program (SPL Token)
                r(pk(DAMM), false),          // 12 referral (None sentinel = program)
                r(pk(DAMM_EVENT_AUTH), false), // 13 event authority
                r(pk(DAMM), false),          // 14 DEX program
            ],
        ),
        _ => (
            0x03,
            vec![
                r(pk(CPMM), false),          // 0 DEX program
                AccountMeta::new(bot.pubkey(), true), // 1 payer
                r(pk(CPMM_AUTHORITY), false), // 2 authority
                r(pk(AMMCFG), false),        // 3 amm_config
                w(pk(POOL), false),          // 4 pool_state
                w(wsol_ata, false),          // 5 input_ta (WSOL)
                w(out_ata, false),           // 6 output_ta (token)
                w(pk(VAULT_WSOL), false),    // 7 input_vault (WSOL)
                w(pk(VAULT_TOK), false),     // 8 output_vault (token)
                r(pk(TOKEN), false),         // 9 input_token_program
                r(pk(TOKEN2022), false),     // 10 output_token_program
                r(wsol, false),              // 11 input_mint (WSOL)
                r(pk(MINT_TOK), false),      // 12 output_mint (token)
                w(pk(OBS), false),           // 13 observation
                r(pk(CPMM), false),          // 14 DEX program
            ],
        ),
    };
    let n_accounts = hop.len() as u8;
    let mut data = vec![0x01u8];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(1);
    data.extend_from_slice(&[venue_code, 0, n_accounts]);

    let mut accounts = prefix;
    accounts.extend(hop);
    let route_ix = Instruction { program_id, accounts, data };

    let bh = rpc.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(&[route_ix], Some(&bot.pubkey()), &[&bot], bh);
    let sim = rpc.simulate_transaction(&tx).unwrap();
    println!("\n=== ROUTE SIMULATION ({venue}: WSOL→token via our CPI into the real pool) ===");
    println!("err (solana-sdk path): {:?}", sim.value.err);
    for l in sim.value.logs.clone().unwrap_or_default() {
        println!("  {l}");
    }

    // === TEST OF OUR SUBMIT LAYER: the SAME tx through send::submit + feed::submit ===
    use feed::run::Submitter;
    use send::package::{Blockhash, Channel, SignablePackage};
    use send::submit::Signer as OurSigner;

    struct Mock(pub [u8; 32]);
    impl OurSigner for Mock {
        fn pubkey(&self) -> [u8; 32] {
            self.0
        }
        fn sign_message(&self, _m: &[u8]) -> [u8; 64] {
            [0u8; 64]
        }
    }

    let pkg = SignablePackage {
        message: tx.message.serialize(),
        fee_payer: bot.pubkey().to_bytes(),
        blockhash_mode: Blockhash::Recent,
        expected_gross: 0,
        fixed_cost: 5_000,
        tip: 0,
        channel: Channel::PublicTpu,
        cu_limit: 0,
        cu_price_micro: 0,
    };
    let signer = Mock(bot.pubkey().to_bytes());
    let rpc_src = feed::transport::CurlSource::new("http://127.0.0.1:8899");
    let mut submitter = feed::submit::SimulatingSubmitter::new(&signer, &rpc_src);
    submitter.submit(pkg);
    let o = submitter.last().unwrap();
    println!("\n=== OUR SUBMIT LAYER: SimulatingSubmitter → simulateTransaction (WITHOUT sending) ===");
    println!("rpc_ok={} tx_len={} CU={:?}", o.rpc_ok, o.tx_len, o.units_consumed);
    println!("err (our path): {:?}", o.err);

    let sdk_err = format!("{:?}", sim.value.err);
    let our_err = o.err.clone().unwrap_or_default();
    let is_c0 = |s: &str| s.contains("Custom(0)") || s.contains("\"Custom\":0");
    println!(
        "\n>>> AGREEMENT: solana-sdk Custom(0)={} · our submit Custom(0)={} · both paths agree={}",
        is_c0(&sdk_err),
        is_c0(&our_err),
        is_c0(&sdk_err) && is_c0(&our_err) && o.rpc_ok
    );
}
