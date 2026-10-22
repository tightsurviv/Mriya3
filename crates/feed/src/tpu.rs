//! Sending to the **leader TPU** (QUIC) — skeleton.
//!
//! Instead of the `sendTransaction` RPC, a transaction can go straight to the TPU port of the current and a few
//! next leaders (fanout) — lower latency, SWQoS path. This module:
//! - **prepares** the package (signature via the [`Signer`] seam + assembling the tx bytes),
//! - **computes the TPU endpoints** of leaders from the schedule (`getLeaderSchedule` + `getClusterNodes`),
//! - hands the package to the [`TpuSender`] seam.
//!
//! **Security boundary:** the real QUIC submit is implemented by the operator (QUIC client, e.g.
//! `quinn`/`solana-tpu-client`). The default [`DryRunTpuSender`] **sends nothing** — it only records
//! what would go where. The core (schedule parsing, leader selection, fanout, package preparation)
//! is pure and testable without a network. This is only the public channel (TPU/SWQoS).

use crate::run::Submitter;
use crate::submit::RpcClient;
use send::package::SignablePackage;
use send::submit::{sign_package, Signer};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Cluster node: pubkey (base58) + TPU address for QUIC (`ip:port`), if advertised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterNode {
    pub pubkey: String,
    pub tpu_quic: Option<String>,
}

/// Parses `getClusterNodes` → nodes with a TPU-QUIC address (prefers `tpuQuic`, falls back to `tpu`).
pub fn parse_cluster_nodes(resp: &Value) -> Vec<ClusterNode> {
    let Some(arr) = resp.get("result").and_then(|r| r.as_array()) else { return vec![] };
    arr.iter()
        .filter_map(|n| {
            let pubkey = n.get("pubkey")?.as_str()?.to_string();
            let tpu_quic = n
                .get("tpuQuic")
                .and_then(|v| v.as_str())
                .or_else(|| n.get("tpu").and_then(|v| v.as_str()))
                .map(String::from);
            Some(ClusterNode { pubkey, tpu_quic })
        })
        .collect()
}

/// Parses `getLeaderSchedule` (`{pubkey: [relSlot...]}`) + the first slot of the epoch → map
/// **absolute slot → leader pubkey**. `epoch_first_slot` = `absoluteSlot − slotIndex` (from getEpochInfo).
pub fn parse_leader_schedule(resp: &Value, epoch_first_slot: u64) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let Some(map) = resp.get("result").and_then(|r| r.as_object()) else { return out };
    for (pubkey, slots) in map {
        let Some(arr) = slots.as_array() else { continue };
        for s in arr {
            if let Some(rel) = s.as_u64() {
                out.insert(epoch_first_slot + rel, pubkey.clone());
            }
        }
    }
    out
}

/// Ordered, **unique** leaders for slots `[current_slot, current_slot + count)`.
/// Order = ascending slots; duplicates (same leader in adjacent slots) merged, first one wins.
pub fn next_leaders(schedule: &HashMap<u64, String>, current_slot: u64, count: u64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for slot in current_slot..current_slot + count {
        if let Some(pk) = schedule.get(&slot) {
            if !out.contains(pk) {
                out.push(pk.clone());
            }
        }
    }
    out
}

/// Maps leader pubkeys to TPU-QUIC addresses (dedup, skips unknown nodes and those without an address).
pub fn resolve_tpu_endpoints(leaders: &[String], nodes: &[ClusterNode]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for leader in leaders {
        if let Some(node) = nodes.iter().find(|n| &n.pubkey == leader) {
            if let Some(ep) = &node.tpu_quic {
                if !out.contains(ep) {
                    out.push(ep.clone());
                }
            }
        }
    }
    out
}

/// Fetches and assembles TPU endpoints for the fanout: `getLeaderSchedule` + `getClusterNodes` over RPC.
/// Network shell around the tested parsers. `epoch_first_slot`/`current_slot` are supplied by
/// the caller (from `getEpochInfo`). Returns the endpoints of the current and next leaders.
pub fn fetch_target_endpoints(
    rpc: &dyn RpcClient,
    epoch_first_slot: u64,
    current_slot: u64,
    fanout: u64,
) -> Result<Vec<String>, String> {
    let sched = rpc.call("getLeaderSchedule", json!([current_slot])).ok_or("no response from getLeaderSchedule")?;
    let schedule = parse_leader_schedule(&sched, epoch_first_slot);
    let leaders = next_leaders(&schedule, current_slot, fanout);
    let nodes_resp = rpc.call("getClusterNodes", json!([])).ok_or("no response from getClusterNodes")?;
    let nodes = parse_cluster_nodes(&nodes_resp);
    Ok(resolve_tpu_endpoints(&leaders, &nodes))
}

/// Seam for sending a packet to the TPU — implemented by the **operator** (real QUIC client). The core only calls it.
pub trait TpuSender {
    /// Sends raw transaction bytes to a TPU-QUIC endpoint (`ip:port`). `Ok(())` = accepted for sending.
    fn send_packet(&mut self, endpoint: &str, tx: &[u8]) -> Result<(), String>;
}

/// Default "dry-run" sender: **sends nothing**, only records `(endpoint, tx_length)`.
/// Lets you walk the whole path (signature → fanout → packets) without a network and without a real submit.
#[derive(Default, Debug)]
pub struct DryRunTpuSender {
    pub packets: Vec<(String, usize)>,
}

impl TpuSender for DryRunTpuSender {
    fn send_packet(&mut self, endpoint: &str, tx: &[u8]) -> Result<(), String> {
        self.packets.push((endpoint.to_string(), tx.len()));
        Ok(())
    }
}

/// Result of an attempt to send one opportunity to the TPU.
#[derive(Clone, Debug)]
pub struct TpuOutcome {
    pub expected_gross: u64,
    pub tx_len: usize,
    /// Endpoints the packet was directed to (fanout).
    pub endpoints: Vec<String>,
    /// How many endpoints accepted the packet (sender returned Ok).
    pub accepted: usize,
    /// Signing/assembly error (then `tx_len==0`), or the last sender error.
    pub err: Option<String>,
}

/// Submitter that routes a transaction to the leaders' TPU: signs (the [`Signer`] seam), assembles the tx and sends it
/// to `endpoints` via [`TpuSender`]. It does NOT open connections itself — the sender (operator) does.
pub struct TpuSubmitter<'a, S: Signer, T: TpuSender> {
    pub signer: &'a S,
    pub sender: T,
    /// TPU-QUIC endpoints of the current and next leaders (refreshed by [`fetch_target_endpoints`]).
    pub endpoints: Vec<String>,
    pub outcomes: Vec<TpuOutcome>,
}

impl<'a, S: Signer, T: TpuSender> TpuSubmitter<'a, S, T> {
    pub fn new(signer: &'a S, sender: T, endpoints: Vec<String>) -> Self {
        Self { signer, sender, endpoints, outcomes: Vec::new() }
    }

    pub fn last(&self) -> Option<&TpuOutcome> {
        self.outcomes.last()
    }
}

impl<'a, S: Signer, T: TpuSender> Submitter for TpuSubmitter<'a, S, T> {
    fn submit(&mut self, pkg: SignablePackage) {
        let tx = match sign_package(self.signer, &pkg) {
            Ok(t) => t,
            Err(_) => {
                self.outcomes.push(TpuOutcome {
                    expected_gross: pkg.expected_gross,
                    tx_len: 0,
                    endpoints: vec![],
                    accepted: 0,
                    err: Some("signing/assembly failed".into()),
                });
                return;
            }
        };
        let endpoints = self.endpoints.clone();
        let mut accepted = 0;
        let mut err = None;
        for ep in &endpoints {
            match self.sender.send_packet(ep, &tx) {
                Ok(()) => accepted += 1,
                Err(e) => err = Some(e),
            }
        }
        self.outcomes.push(TpuOutcome {
            expected_gross: pkg.expected_gross,
            tx_len: tx.len(),
            endpoints,
            accepted,
            err,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use send::package::{Blockhash, Channel, SignablePackage};
    use send::submit::Signer as OurSigner;
    use send::tx::{build_arb_message, AccountMeta, Pubkey};

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    struct MockSigner {
        pk: Pubkey,
    }
    impl OurSigner for MockSigner {
        fn pubkey(&self) -> Pubkey {
            self.pk
        }
        fn sign_message(&self, _m: &[u8]) -> [u8; 64] {
            [1u8; 64]
        }
    }

    fn package(fee_payer: Pubkey) -> SignablePackage {
        let route_accounts = vec![
            AccountMeta { pubkey: fee_payer, is_signer: true, is_writable: true },
            AccountMeta { pubkey: id(2), is_signer: false, is_writable: true },
        ];
        let message = build_arb_message(
            fee_payer, id(200), id(100), vec![0x01, 0, 0, 0, 0, 0, 0, 0, 0], route_accounts, 127_000, 131_072, 61_825, None, &[],
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
    fn parses_cluster_nodes_prefers_quic() {
        let resp = json!({ "result": [
            { "pubkey": "VAL1", "tpu": "1.1.1.1:8003", "tpuQuic": "1.1.1.1:8009" },
            { "pubkey": "VAL2", "tpu": "2.2.2.2:8003" },            // no tpuQuic → fallback to tpu
            { "pubkey": "VAL3" }                                      // no address → None
        ]});
        let nodes = parse_cluster_nodes(&resp);
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].tpu_quic.as_deref(), Some("1.1.1.1:8009"));
        assert_eq!(nodes[1].tpu_quic.as_deref(), Some("2.2.2.2:8003"));
        assert_eq!(nodes[2].tpu_quic, None);
    }

    #[test]
    fn leader_schedule_to_absolute_slots_and_next_leaders() {
        // the epoch starts at slot 1000; VAL1 has relSlot 0,1; VAL2 has relSlot 2,3.
        let resp = json!({ "result": { "VAL1": [0, 1], "VAL2": [2, 3] } });
        let sched = parse_leader_schedule(&resp, 1000);
        assert_eq!(sched.get(&1000).map(String::as_str), Some("VAL1"));
        assert_eq!(sched.get(&1002).map(String::as_str), Some("VAL2"));
        // From slot 1001, 3 slots → VAL1(1001), VAL2(1002,1003) → unique [VAL1, VAL2].
        assert_eq!(next_leaders(&sched, 1001, 3), vec!["VAL1".to_string(), "VAL2".to_string()]);
    }

    #[test]
    fn resolves_endpoints_dedup_and_skips_unknown() {
        let nodes = vec![
            ClusterNode { pubkey: "VAL1".into(), tpu_quic: Some("1.1.1.1:8009".into()) },
            ClusterNode { pubkey: "VAL2".into(), tpu_quic: None },
        ];
        // VAL1 → endpoint; VAL2 → no address (skip); VALX → unknown (skip); VAL1 again → dedup.
        let eps = resolve_tpu_endpoints(&["VAL1".into(), "VAL2".into(), "VALX".into(), "VAL1".into()], &nodes);
        assert_eq!(eps, vec!["1.1.1.1:8009".to_string()]);
    }

    #[test]
    fn tpu_submitter_signs_and_fans_out_dry_run() {
        let fee_payer = id(3);
        let signer = MockSigner { pk: fee_payer };
        let endpoints = vec!["1.1.1.1:8009".to_string(), "2.2.2.2:8009".to_string()];
        let mut sub = TpuSubmitter::new(&signer, DryRunTpuSender::default(), endpoints.clone());
        sub.submit(package(fee_payer));

        let o = sub.last().unwrap();
        assert!(o.tx_len > 64, "tx assembled (signature + message)");
        assert_eq!(o.endpoints, endpoints);
        assert_eq!(o.accepted, 2, "both endpoints accepted (dry-run)");
        assert!(o.err.is_none());
        // Nothing actually went out — packets only recorded by DryRunTpuSender.
        assert_eq!(sub.sender.packets.len(), 2);
        assert_eq!(sub.sender.packets[0].0, "1.1.1.1:8009");
        assert!(sub.sender.packets[0].1 > 64);
    }

    #[test]
    fn wrong_signer_records_failure_no_packets() {
        let signer = MockSigner { pk: id(9) }; // ≠ fee_payer
        let mut sub = TpuSubmitter::new(&signer, DryRunTpuSender::default(), vec!["1.1.1.1:8009".into()]);
        sub.submit(package(id(3)));
        let o = sub.last().unwrap();
        assert_eq!(o.tx_len, 0);
        assert_eq!(o.accepted, 0);
        assert!(o.err.is_some());
        assert_eq!(sub.sender.packets.len(), 0, "wrong signature → no packet went out");
    }
}
