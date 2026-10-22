//! Live transport: an account source ([`AccountSource`]) fetching data through the system `curl`
//! (TLS from the system certificate store; no Rust TLS stack is bundled).
//!
//! `CurlSource` fetches accounts in bulk (`getMultipleAccounts`) and caches them, so `get_account`
//! stays cheap and network-free. Fetching and selection are **separated from parsing**, so that
//! the logic (response parser, tick array selection) is testable without a network. This is READ-ONLY —
//! it sends no transaction; signing and submit remain on the operator's side.

use crate::AccountSource;
use base64::Engine;
use pools::Pubkey;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Decodes the base-58 representation of a key into 32 bytes (`None` if the length ≠ 32).
pub fn pubkey_from_b58(s: &str) -> Option<Pubkey> {
    let v = bs58::decode(s).into_vec().ok()?;
    if v.len() != 32 {
        return None;
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&v);
    Some(k)
}

fn b64(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Parses the `getMultipleAccounts` result (`result.value[]`) → account data in the order of `keys`.
/// `null` entries (account does not exist) yield `None`. Pure — testable without a network.
pub fn parse_get_multiple_accounts(resp: &Value, n: usize) -> Vec<Option<Vec<u8>>> {
    let arr = resp.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_array());
    let mut out = vec![None; n];
    if let Some(arr) = arr {
        for (i, a) in arr.iter().enumerate().take(n) {
            if a.is_null() {
                continue;
            }
            if let Some(d) = a.get("data").and_then(|x| x.get(0)).and_then(|x| x.as_str()) {
                out[i] = b64(d);
            }
        }
    }
    out
}

/// Parses the `getProgramAccounts` result → `(pubkey, data)`. Pure — testable without a network.
pub fn parse_program_accounts(resp: &Value) -> Vec<(Pubkey, Vec<u8>)> {
    let Some(arr) = resp.get("result").and_then(|r| r.as_array()) else { return vec![] };
    arr.iter()
        .filter_map(|a| {
            let pk = pubkey_from_b58(a.get("pubkey")?.as_str()?)?;
            let d = a.get("account")?.get("data")?.get(0)?.as_str()?;
            Some((pk, b64(d)?))
        })
        .collect()
}

/// Selects tick arrays to pass: starting from the array containing `current_tick` in the swap direction, only
/// **existing** (initialized) accounts from `available` (start, pubkey), up to `count` items.
/// Passing a non-existent array account would break the tx — so we take only the available ones.
pub fn select_tick_arrays(
    available: &[(i32, Pubkey)],
    current_tick: i32,
    tick_spacing: u16,
    ascending: bool,
    count: u32,
) -> Vec<Pubkey> {
    let span = tick_spacing as i32 * 60;
    let cur = current_tick.div_euclid(span) * span;
    let mut cand: Vec<(i32, Pubkey)> =
        available.iter().copied().filter(|(s, _)| if ascending { *s >= cur } else { *s <= cur }).collect();
    cand.sort_by_key(|(s, _)| if ascending { *s } else { -*s });
    cand.into_iter().take(count as usize).map(|(_, pk)| pk).collect()
}

/// Account source via `curl` + a cache. Fetching is explicit (`prefetch`/`fetch_*`), and `get_account`
/// reads only from the cache.
pub struct CurlSource {
    url: String,
    cache: HashMap<Pubkey, Vec<u8>>,
}

impl CurlSource {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), cache: HashMap::new() }
    }

    /// How many accounts are in the cache.
    pub fn len(&self) -> usize {
        self.cache.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
    /// Inserts an account into the cache manually (tests / data from another source).
    pub fn insert(&mut self, key: Pubkey, data: Vec<u8>) {
        self.cache.insert(key, data);
    }

    /// Raw JSON-RPC call via `curl` (body on stdin). Read-only.
    pub fn rpc(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        let mut child = Command::new("curl")
            .args(["-sS", "-m", "30", "-X", "POST", "-H", "content-type: application/json", "--data-binary", "@-", &self.url])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        child.stdin.as_mut()?.write_all(body.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        serde_json::from_slice(&out.stdout).ok()
    }

    /// Fetches accounts in bulk and caches them. Returns the number of fetched (non-empty) accounts.
    pub fn prefetch(&mut self, keys: &[Pubkey]) -> Result<usize, String> {
        if keys.is_empty() {
            return Ok(0);
        }
        let b58: Vec<String> = keys.iter().map(|k| bs58::encode(k).into_string()).collect();
        let resp = self
            .rpc("getMultipleAccounts", json!([b58, {"encoding":"base64"}]))
            .ok_or("no RPC response (curl)")?;
        let datas = parse_get_multiple_accounts(&resp, keys.len());
        let mut n = 0;
        for (k, d) in keys.iter().zip(datas) {
            if let Some(bytes) = d {
                self.cache.insert(*k, bytes);
                n += 1;
            }
        }
        Ok(n)
    }

    /// Fetches the tick arrays of a CLMM/Whirlpool pool via `getProgramAccounts` (memcmp pool@offset).
    /// Returns `(start_index, pubkey)` for each decoded array (Raydium CLMM: pool@8).
    pub fn fetch_clmm_tick_arrays(&self, program: &Pubkey, pool: &Pubkey) -> Result<Vec<(i32, Pubkey)>, String> {
        let prog = bs58::encode(program).into_string();
        let poolb = bs58::encode(pool).into_string();
        let params = json!([prog, {"encoding":"base64","filters":[{"memcmp":{"offset":8,"bytes":poolb}}]}]);
        let resp = self.rpc("getProgramAccounts", params).ok_or("no RPC response (curl)")?;
        let mut out = Vec::new();
        for (pk, data) in parse_program_accounts(&resp) {
            if let Some((start, _ticks)) = pools::clmm::decode_tick_array(&data) {
                out.push((start, pk));
            }
        }
        Ok(out)
    }
}

impl AccountSource for CurlSource {
    fn get_account(&self, key: &Pubkey) -> Option<&[u8]> {
        self.cache.get(key).map(|v| v.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_multiple_accounts_with_null() {
        // Two accounts: the first exists (base64 "AQID" = [1,2,3]), the second is null.
        let resp = json!({
            "result": { "value": [
                { "data": ["AQID", "base64"], "owner": "x" },
                null
            ]}
        });
        let out = parse_get_multiple_accounts(&resp, 2);
        assert_eq!(out[0], Some(vec![1, 2, 3]));
        assert_eq!(out[1], None);
    }

    #[test]
    fn parses_program_accounts() {
        let resp = json!({
            "result": [
                { "pubkey": "11111111111111111111111111111111", "account": { "data": ["AQID", "base64"] } }
            ]
        });
        let got = parse_program_accounts(&resp);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, [0u8; 32]); // System = 32 zeros
        assert_eq!(got[0].1, vec![1, 2, 3]);
    }

    #[test]
    fn selects_initialized_tick_arrays_in_direction() {
        let a = |n: u8| {
            let mut k = [0u8; 32];
            k[0] = n;
            k
        };
        // Available (initialized) arrays: starts -55800,-55200,-54600 (step 600, spacing 10).
        let avail = vec![(-55800, a(1)), (-55200, a(2)), (-54600, a(3))];
        // current_tick -55000 → current array -55200; upward, 2 items → [-55200,-54600].
        let up = select_tick_arrays(&avail, -55000, 10, true, 2);
        assert_eq!(up, vec![a(2), a(3)]);
        // downward, 2 items → [-55200,-55800].
        let down = select_tick_arrays(&avail, -55000, 10, false, 2);
        assert_eq!(down, vec![a(2), a(1)]);
        // a non-existent neighbour is skipped: without -54600, upward returns only [-55200].
        let avail2 = vec![(-55200, a(2))];
        assert_eq!(select_tick_arrays(&avail2, -55000, 10, true, 2), vec![a(2)]);
    }

    #[test]
    fn curl_source_is_account_source_over_cache() {
        let mut s = CurlSource::new("http://unused");
        s.insert([7u8; 32], vec![9, 9, 9]);
        assert_eq!(s.len(), 1);
        assert_eq!(s.get_account(&[7u8; 32]), Some(&[9u8, 9, 9][..]));
        assert_eq!(s.get_account(&[8u8; 32]), None);
    }
}
