//! Read-only demo: fetches pool accounts from RPC, decodes and prices them — the whole chain live.
//! Does NOT send any transaction. RPC URL (with a key, if any) comes from env `SOLANA_RPC_URL` (not in the repo).
//!
//! TLS is handled by the system `curl` (the system certificate store).

use base64::Engine;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// JSON-RPC via the system `curl` (body on stdin, URL with the key in the argument).
fn rpc(url: &str, method: &str, params: Value) -> Option<Value> {
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let mut child = Command::new("curl")
        .args(["-sS", "-m", "30", "-X", "POST", "-H", "content-type: application/json", "--data-binary", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(body.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

fn b58(s: &str) -> [u8; 32] {
    let v = bs58::decode(s).into_vec().expect("b58");
    let mut k = [0u8; 32];
    k.copy_from_slice(&v);
    k
}

fn account_data(url: &str, pubkey: &str) -> Option<Vec<u8>> {
    let r = rpc(url, "getAccountInfo", json!([pubkey, {"encoding":"base64"}]))?;
    let d = r.get("result")?.get("value")?.get("data")?.get(0)?.as_str()?;
    base64::engine::general_purpose::STANDARD.decode(d).ok()
}

/// Program accounts with a memcmp filter (returns (address, data)).
fn program_accounts(url: &str, program: &str, offset: u64, b58: &str) -> Vec<Vec<u8>> {
    let params = json!([program, {"encoding":"base64","filters":[{"memcmp":{"offset":offset,"bytes":b58}}]}]);
    let Some(r) = rpc(url, "getProgramAccounts", params) else { return vec![] };
    r.get("result")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let d = a.get("account")?.get("data")?.get(0)?.as_str()?;
                    base64::engine::general_purpose::STANDARD.decode(d).ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn main() {
    let url = match std::env::var("SOLANA_RPC_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("Set SOLANA_RPC_URL (full RPC URL, with a key if the provider requires one). Sending nothing.");
            std::process::exit(2);
        }
    };
    println!("== Read-only demo (no sending) ==\n");

    // 1) Raydium CLMM
    let clmm_addr = "C6ZW98DYKwk6CrpysfDVheJkyz1EY4XGRL5LbQniPK5w";
    if let Some(d) = account_data(&url, clmm_addr) {
        if let Some(p) = pools::clmm::decode(&d) {
            let price = p.sqrt_price_f64() * p.sqrt_price_f64();
            println!("Raydium CLMM {}", clmm_addr);
            println!("  tick={} liquidity={} raw_price(token1/token0)={:.10}", p.tick_current, p.liquidity, price);
            let pool = feed::clmm_pool_from_account(b58(clmm_addr), &d, 25).unwrap();
            println!("  quote 1e6 token0→token1 = {}", pool.quote(1_000_000, true));

            // Find this pool's active TickArray and compute a large swap with tick crossing.
            let ray_prog = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
            let arrays = program_accounts(&url, ray_prog, 8, clmm_addr);
            let span = 60i32 * p.tick_spacing as i32;
            let active = arrays.iter().find(|a| {
                pools::clmm::decode_tick_array(a)
                    .map(|(start, _)| start <= p.tick_current && p.tick_current < start + span)
                    .unwrap_or(false)
            });
            if let Some(arr) = active {
                let pool_tx = feed::clmm_pool_with_ticks(b58(clmm_addr), &d, arr, 25).unwrap();
                let big = 1_000_000_000_000u64;
                let single = pool.quote(big, true);
                let crossing = pool_tx.quote(big, true);
                let n = pools::clmm::decode_tick_array(arr).map(|(_, t)| t.len()).unwrap_or(0);
                println!("  large swap {}: single-range={} vs with tick crossing={} ({} ticks)", big, single, crossing, n);
            }
            println!();
        }
    }

    // 2) Pump AMM (constant product): pool → vaults → reserves → pricing
    let pump_addr = "JEBqxuDB3isvH1wf2Dpa9Q9P6BWUsZSAJnNU6ksz4rpB";
    if let Some(d) = account_data(&url, pump_addr) {
        if let Some(p) = pools::pump_amm::decode(&d) {
            let bv = bs58::encode(p.base_vault).into_string();
            let qv = bs58::encode(p.quote_vault).into_string();
            let base_r = account_data(&url, &bv).and_then(|x| pools::parse_token_amount(&x)).unwrap_or(0);
            let quote_r = account_data(&url, &qv).and_then(|x| pools::parse_token_amount(&x)).unwrap_or(0);
            println!("Pump AMM {}", pump_addr);
            println!("  base_reserve={} quote_reserve={} (quote=WSOL:{})", base_r, quote_r, p.quote_mint == pools::WSOL);
            let cp = p.to_cp(base_r, quote_r, 25, true);
            println!("  quote 1e6 quote→base = {}\n", sim::amount_out(1_000_000, cp));
        }
    }

    // 3) Orca Whirlpool (CLMM)
    let wp_addr = "JEK9eKmyTtFN7ZhQJ199MSVx849K7sZYgNp6QLVxEk3A";
    if let Some(d) = account_data(&url, wp_addr) {
        if let Some(w) = pools::whirlpool::decode(&d) {
            let price = w.sqrt_price_f64() * w.sqrt_price_f64();
            println!("Orca Whirlpool {}", wp_addr);
            println!("  tick={} fee={}bps raw_price={:.12}", w.tick_current, w.fee_bps(), price);
            let pool = feed::whirlpool_pool_from_account(b58(wp_addr), &d).unwrap();
            println!("  quote 1e6 A→B = {}\n", pool.quote(1_000_000, true));
        }
    }

    println!("Done. (This is only reading and pricing — no transaction was sent.)");
}
