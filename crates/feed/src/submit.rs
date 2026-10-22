//! Dry-run submitter: **signs** the package (with the operator's [`Signer`]) and **simulates** it via RPC
//! (`simulateTransaction`) — **without a real send**.
//!
//! `simulateTransaction` executes the transaction in the RPC node's bank and **commits nothing** — it is a
//! safe check of the full loop (signature + program execution + CPI) on devnet or a local
//! `solana-test-validator`. **A REAL send (`sendTransaction`) is a separate operator step and is
//! deliberately NOT here** — the operator implements it with their own [`crate::run::Submitter`] using a real
//! key and node, outside this module.

use crate::run::Submitter;
use crate::transport::CurlSource;
use base64::Engine as _;
use send::package::SignablePackage;
use send::submit::{sign_package, Signer};
use serde_json::{json, Value};

/// RPC client for simulation (seam: live [`CurlSource`], a mock in tests).
pub trait RpcClient {
    fn call(&self, method: &str, params: Value) -> Option<Value>;
}

impl RpcClient for CurlSource {
    fn call(&self, method: &str, params: Value) -> Option<Value> {
        self.rpc(method, params)
    }
}

/// Simulation result for one opportunity.
#[derive(Clone, Debug)]
pub struct SimOutcome {
    /// Expected gross profit of the package (for logs/comparison with the real execution).
    pub expected_gross: u64,
    /// Size of the assembled wire transaction (0 if signing/assembly failed).
    pub tx_len: usize,
    /// Simulation error: `None` = it would pass (no error), `Some(..)` = it would revert (e.g. `Custom(0)`).
    pub err: Option<String>,
    /// Compute units consumed (from the simulation), if the node reported them.
    pub units_consumed: Option<u64>,
    /// Program logs from the simulation.
    pub logs: Vec<String>,
    /// Whether the RPC node responded at all.
    pub rpc_ok: bool,
}

/// Base64 (standard) of the transaction for the RPC request field.
fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Parses the `simulateTransaction` response → (err, logs, consumed CU). Pure — testable without a network.
pub fn parse_simulation(resp: &Value) -> (Option<String>, Vec<String>, Option<u64>) {
    let v = resp.get("result").and_then(|r| r.get("value"));
    let err = v
        .and_then(|v| v.get("err"))
        .and_then(|e| if e.is_null() { None } else { Some(e.to_string()) });
    let logs = v
        .and_then(|v| v.get("logs"))
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let units = v.and_then(|v| v.get("unitsConsumed")).and_then(|u| u.as_u64());
    (err, logs, units)
}

/// Submitter: **signs + simulates**, collects results. **Sends nothing for real.**
///
/// Simulation parameters: `sigVerify=false` (allows dry-running even without full signature verification
/// on devnet), `replaceRecentBlockhash=false` (we do not replace the nonce/blockhash value).
pub struct SimulatingSubmitter<'a, S: Signer, R: RpcClient> {
    pub signer: &'a S,
    pub rpc: &'a R,
    pub outcomes: Vec<SimOutcome>,
}

impl<'a, S: Signer, R: RpcClient> SimulatingSubmitter<'a, S, R> {
    pub fn new(signer: &'a S, rpc: &'a R) -> Self {
        Self { signer, rpc, outcomes: Vec::new() }
    }

    /// Last result (handy after `submit`).
    pub fn last(&self) -> Option<&SimOutcome> {
        self.outcomes.last()
    }
}

impl<'a, S: Signer, R: RpcClient> Submitter for SimulatingSubmitter<'a, S, R> {
    fn submit(&mut self, pkg: SignablePackage) {
        // 1. Signature (operator's Signer) + assembling the wire transaction.
        let tx = match sign_package(self.signer, &pkg) {
            Ok(t) => t,
            Err(_) => {
                self.outcomes.push(SimOutcome {
                    expected_gross: pkg.expected_gross,
                    tx_len: 0,
                    err: Some("signing/assembly failed".into()),
                    units_consumed: None,
                    logs: vec![],
                    rpc_ok: false,
                });
                return;
            }
        };
        // 2. SIMULATION (not a send). simulateTransaction executes and discards — no commit.
        let params = json!([
            b64(&tx),
            { "encoding": "base64", "sigVerify": false, "replaceRecentBlockhash": false, "commitment": "processed" }
        ]);
        let (err, logs, units, rpc_ok) = match self.rpc.call("simulateTransaction", params) {
            Some(r) => {
                let (e, l, u) = parse_simulation(&r);
                (e, l, u, true)
            }
            None => (Some("no RPC response".into()), vec![], None, false),
        };
        self.outcomes.push(SimOutcome {
            expected_gross: pkg.expected_gross,
            tx_len: tx.len(),
            err,
            units_consumed: units,
            logs,
            rpc_ok,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use send::package::{Blockhash, Channel, SignablePackage};
    use send::tx::{build_arb_message, AccountMeta, Pubkey};
    use std::cell::RefCell;

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    struct MockSigner {
        pk: Pubkey,
    }
    impl Signer for MockSigner {
        fn pubkey(&self) -> Pubkey {
            self.pk
        }
        fn sign_message(&self, _m: &[u8]) -> [u8; 64] {
            [1u8; 64]
        }
    }

    /// RPC mock: returns the given response and records the called method (to confirm that it is a
    /// SIMULATION, not a send).
    struct MockRpc {
        resp: Value,
        last_method: RefCell<String>,
    }
    impl RpcClient for MockRpc {
        fn call(&self, method: &str, _params: Value) -> Option<Value> {
            *self.last_method.borrow_mut() = method.to_string();
            Some(self.resp.clone())
        }
    }

    fn package(fee_payer: Pubkey) -> SignablePackage {
        // v0 message, one signature. The number of signatures is read from the byte after the 0x80 prefix.
        let route_accounts = vec![
            AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
            AccountMeta { pubkey: id(2), is_signer: false, is_writable: true },
        ];
        let message = build_arb_message(
            fee_payer,
            id(200),
            id(100),
            vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0],
            route_accounts,
            127_000,
            131_072,
            61_825,
            None,
            &[],
        );
        SignablePackage {
            message,
            fee_payer,
            blockhash_mode: Blockhash::Recent,
            expected_gross: 120_000,
            fixed_cost: 8_000,
            tip: 0,
            channel: Channel::PublicTpu,
            cu_limit: 127_000,
            cu_price_micro: 61_825,
        }
    }

    #[test]
    fn parse_simulation_success_and_revert() {
        let ok = json!({ "result": { "value": { "err": null, "logs": ["Program log: ok"], "unitsConsumed": 28499 } } });
        let (err, logs, units) = parse_simulation(&ok);
        assert!(err.is_none());
        assert_eq!(units, Some(28499));
        assert_eq!(logs.len(), 1);

        let revert = json!({ "result": { "value": { "err": { "InstructionError": [0, { "Custom": 0 }] }, "logs": [], "unitsConsumed": 900 } } });
        let (err, _l, units) = parse_simulation(&revert);
        assert!(err.is_some(), "revert → err present");
        assert_eq!(units, Some(900));
    }

    #[test]
    fn simulating_submitter_signs_and_simulates_only() {
        let fee_payer = id(3);
        let signer = MockSigner { pk: fee_payer };
        let rpc = MockRpc {
            resp: json!({ "result": { "value": { "err": null, "logs": ["Program log: SwapBaseInput"], "unitsConsumed": 28499 } } }),
            last_method: RefCell::new(String::new()),
        };
        let mut sub = SimulatingSubmitter::new(&signer, &rpc);
        sub.submit(package(fee_payer));

        // Key point: the method is SIMULATION, not a send.
        assert_eq!(*rpc.last_method.borrow(), "simulateTransaction");
        let o = sub.last().unwrap();
        assert!(o.rpc_ok);
        assert!(o.err.is_none(), "simulation without error");
        assert_eq!(o.units_consumed, Some(28499));
        assert!(o.tx_len > 64, "tx assembled (signature + message)");
        assert_eq!(o.expected_gross, 120_000);
    }

    #[test]
    fn wrong_signer_records_failure_without_rpc() {
        let signer = MockSigner { pk: id(9) }; // ≠ fee_payer
        let rpc = MockRpc { resp: json!({}), last_method: RefCell::new(String::new()) };
        let mut sub = SimulatingSubmitter::new(&signer, &rpc);
        sub.submit(package(id(3)));
        let o = sub.last().unwrap();
        assert_eq!(o.tx_len, 0, "no tx with a wrong signature");
        assert!(o.err.is_some());
        assert_eq!(*rpc.last_method.borrow(), "", "RPC not called on a signing error");
    }
}
