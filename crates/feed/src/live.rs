//! Live feed for [`crate::run::PoolBuilder`]: slot-aware account store + transport seam.
//!
//! [`AccountStore`] holds the freshest data of each account together with its **slot** and rejects late
//! updates (older slot). It implements [`crate::AccountSource`], so `build_pools` reads
//! straight from it. [`FeedSource`] is the transport seam — the operator plugs in RPC polling, `accountSubscribe`
//! (WebSocket) or Geyser; there is also a ready RPC adapter here ([`RpcFeedSource`]) built on
//! [`crate::transport::CurlSource`]. [`LiveFeed`] ties the two together and handles **two phases**
//! (after reading the pools the planner knows the derived accounts — the second phase pulls them in by the same mechanism).
//!
//! Snapshot consistency: `getMultipleAccounts` returns a `context.slot` shared by the whole batch, so
//! accounts fetched in one call come from one slot — this addresses "pools drift between reads".

use crate::transport::{parse_get_multiple_accounts, CurlSource};
use crate::AccountSource;
use pools::Pubkey;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Update of a single account from the live source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountUpdate {
    pub pubkey: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
}

/// Slot-aware account store. Holds data + slot per account; an update with an **older** slot is
/// ignored (protects against rolling back state in a race between sources).
#[derive(Default, Debug)]
pub struct AccountStore {
    map: HashMap<Pubkey, (Vec<u8>, u64)>,
    last_slot: u64,
}

impl AccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies an update. Returns `true` when accepted (new account or slot ≥ the existing one).
    pub fn apply(&mut self, u: AccountUpdate) -> bool {
        if let Some((_, s)) = self.map.get(&u.pubkey) {
            if *s > u.slot {
                return false; // late
            }
        }
        if u.slot > self.last_slot {
            self.last_slot = u.slot;
        }
        self.map.insert(u.pubkey, (u.data, u.slot));
        true
    }

    /// Applies a batch; returns the number accepted.
    pub fn apply_batch(&mut self, updates: Vec<AccountUpdate>) -> usize {
        let mut n = 0;
        for u in updates {
            if self.apply(u) {
                n += 1;
            }
        }
        n
    }

    pub fn slot_of(&self, key: &Pubkey) -> Option<u64> {
        self.map.get(key).map(|(_, s)| *s)
    }

    pub fn last_slot(&self) -> u64 {
        self.last_slot
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Keys missing from the store (to be pulled in).
    pub fn missing(&self, keys: &[Pubkey]) -> Vec<Pubkey> {
        keys.iter().filter(|k| !self.map.contains_key(*k)).copied().collect()
    }

    /// Whether `keys` form a consistent snapshot: all present and slot spread ≤ `tol`.
    /// `tol=0` requires exactly the same slot for all.
    pub fn is_consistent(&self, keys: &[Pubkey], tol: u64) -> bool {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for k in keys {
            match self.map.get(k) {
                Some((_, s)) => {
                    lo = lo.min(*s);
                    hi = hi.max(*s);
                }
                None => return false,
            }
        }
        keys.is_empty() || hi - lo <= tol
    }
}

impl AccountSource for AccountStore {
    fn get_account(&self, key: &Pubkey) -> Option<&[u8]> {
        self.map.get(key).map(|(d, _)| d.as_slice())
    }
}

/// Live transport seam: returns account updates for the given keys. Implemented by the **operator**
/// (RPC poll, `accountSubscribe` WebSocket, Geyser gRPC). The RPC adapter is here ([`RpcFeedSource`]).
pub trait FeedSource {
    fn fetch(&mut self, keys: &[Pubkey]) -> Result<Vec<AccountUpdate>, String>;
}

/// Extracts `result.context.slot` from an RPC response (e.g. getMultipleAccounts). 0 when absent.
pub fn parse_context_slot(resp: &Value) -> u64 {
    resp.get("result")
        .and_then(|r| r.get("context"))
        .and_then(|c| c.get("slot"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0)
}

/// [`FeedSource`] adapter over `getMultipleAccounts` (CurlSource). The slot is taken from the response's
/// `context.slot` — the whole batch is from one slot (consistent snapshot). Read only, no sending.
pub struct RpcFeedSource<'a> {
    pub rpc: &'a CurlSource,
}

impl<'a> RpcFeedSource<'a> {
    pub fn new(rpc: &'a CurlSource) -> Self {
        Self { rpc }
    }
}

impl<'a> FeedSource for RpcFeedSource<'a> {
    fn fetch(&mut self, keys: &[Pubkey]) -> Result<Vec<AccountUpdate>, String> {
        if keys.is_empty() {
            return Ok(vec![]);
        }
        let b58: Vec<String> = keys.iter().map(|k| bs58::encode(k).into_string()).collect();
        let resp = self
            .rpc
            .rpc("getMultipleAccounts", json!([b58, {"encoding":"base64"}]))
            .ok_or("no RPC response (getMultipleAccounts)")?;
        let slot = parse_context_slot(&resp);
        let datas = parse_get_multiple_accounts(&resp, keys.len());
        let mut out = Vec::new();
        for (k, d) in keys.iter().zip(datas) {
            if let Some(data) = d {
                out.push(AccountUpdate { pubkey: *k, data, slot });
            }
        }
        Ok(out)
    }
}

/// Feed loop: source + store. Refreshes the store for the given keys; handles two phases.
pub struct LiveFeed<F: FeedSource> {
    pub source: F,
    pub store: AccountStore,
}

impl<F: FeedSource> LiveFeed<F> {
    pub fn new(source: F) -> Self {
        Self { source, store: AccountStore::new() }
    }

    /// One phase: fetch `keys` from the source and merge them into the store. Returns the number accepted.
    pub fn refresh_keys(&mut self, keys: &[Pubkey]) -> Result<usize, String> {
        let updates = self.source.fetch(keys)?;
        Ok(self.store.apply_batch(updates))
    }

    /// Two-phase refresh: phase 1 = `primary` (pool accounts), phase 2 = `derive(&store)` (derived
    /// accounts computed from phase 1 — e.g. the active DLMM BinArray, CLMM tick arrays). Returns
    /// `(accepted_phase1, accepted_phase2)`.
    pub fn refresh_two_phase(
        &mut self,
        primary: &[Pubkey],
        derive: impl Fn(&AccountStore) -> Vec<Pubkey>,
    ) -> Result<(usize, usize), String> {
        let n1 = self.refresh_keys(primary)?;
        let secondary = derive(&self.store);
        let n2 = if secondary.is_empty() { 0 } else { self.refresh_keys(&secondary)? };
        Ok((n1, n2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> Pubkey {
        let mut a = [0u8; 32];
        a[0] = n;
        a
    }

    fn upd(n: u8, byte: u8, slot: u64) -> AccountUpdate {
        AccountUpdate { pubkey: id(n), data: vec![byte], slot }
    }

    #[test]
    fn apply_respects_slot_order() {
        let mut s = AccountStore::new();
        assert!(s.apply(upd(1, 10, 100)), "new account accepted");
        assert!(!s.apply(upd(1, 11, 99)), "older slot rejected");
        assert_eq!(s.get_account(&id(1)), Some(&[10u8][..]), "data unchanged");
        assert!(s.apply(upd(1, 12, 100)), "same slot = refresh accepted");
        assert_eq!(s.get_account(&id(1)), Some(&[12u8][..]));
        assert!(s.apply(upd(1, 13, 101)), "newer slot accepted");
        assert_eq!(s.last_slot(), 101);
    }

    #[test]
    fn consistency_and_missing() {
        let mut s = AccountStore::new();
        s.apply(upd(1, 1, 500));
        s.apply(upd(2, 2, 501));
        let keys = [id(1), id(2)];
        assert!(s.missing(&keys).is_empty());
        assert!(s.is_consistent(&keys, 1), "spread 1 ≤ tol 1");
        assert!(!s.is_consistent(&keys, 0), "different slots → inconsistent at tol 0");
        assert_eq!(s.missing(&[id(1), id(9)]), vec![id(9)]);
        assert!(!s.is_consistent(&[id(1), id(9)], 100), "missing account → inconsistent");
    }

    #[test]
    fn as_account_source() {
        let mut s = AccountStore::new();
        s.apply(AccountUpdate { pubkey: id(7), data: vec![9, 9, 9], slot: 1 });
        assert_eq!(s.get_account(&id(7)), Some(&[9u8, 9, 9][..]));
        assert_eq!(s.get_account(&id(8)), None);
    }

    #[test]
    fn parse_context_slot_reads_batch_slot() {
        let resp = json!({ "result": { "context": { "slot": 123456 }, "value": [] } });
        assert_eq!(parse_context_slot(&resp), 123456);
        assert_eq!(parse_context_slot(&json!({ "result": {} })), 0);
    }

    /// Source stub: returns pre-set updates for the requested keys (per call).
    struct MockFeed {
        batches: Vec<Vec<AccountUpdate>>,
        calls: Vec<Vec<Pubkey>>,
    }
    impl FeedSource for MockFeed {
        fn fetch(&mut self, keys: &[Pubkey]) -> Result<Vec<AccountUpdate>, String> {
            self.calls.push(keys.to_vec());
            Ok(if self.batches.is_empty() { vec![] } else { self.batches.remove(0) })
        }
    }

    #[test]
    fn live_feed_refresh_populates_store() {
        let feed_src = MockFeed {
            batches: vec![vec![upd(1, 42, 900), upd(2, 43, 900)]],
            calls: vec![],
        };
        let mut lf = LiveFeed::new(feed_src);
        let n = lf.refresh_keys(&[id(1), id(2)]).unwrap();
        assert_eq!(n, 2);
        assert_eq!(lf.store.get_account(&id(1)), Some(&[42u8][..]));
        assert!(lf.store.is_consistent(&[id(1), id(2)], 0), "one batch = one slot");
    }

    #[test]
    fn two_phase_derives_secondary_keys_from_first() {
        // Phase 1 returns account 1 (slot 900). derive() looks at the store and requests the derived account (2).
        let feed_src = MockFeed {
            batches: vec![vec![upd(1, 1, 900)], vec![upd(2, 2, 900)]],
            calls: vec![],
        };
        let mut lf = LiveFeed::new(feed_src);
        let (n1, n2) = lf
            .refresh_two_phase(&[id(1)], |store| {
                // when account 1 is present, pull in derived account 2 (e.g. the active BinArray)
                if store.get_account(&id(1)).is_some() { vec![id(2)] } else { vec![] }
            })
            .unwrap();
        assert_eq!((n1, n2), (1, 1));
        assert_eq!(lf.store.get_account(&id(2)), Some(&[2u8][..]));
        // The second phase actually queried for the derived key.
        assert_eq!(lf.source.calls.len(), 2);
        assert_eq!(lf.source.calls[1], vec![id(2)]);
    }

    #[test]
    fn two_phase_skips_second_when_no_derived_keys() {
        let feed_src = MockFeed { batches: vec![vec![upd(1, 1, 900)]], calls: vec![] };
        let mut lf = LiveFeed::new(feed_src);
        let (n1, n2) = lf.refresh_two_phase(&[id(1)], |_| vec![]).unwrap();
        assert_eq!((n1, n2), (1, 0));
        assert_eq!(lf.source.calls.len(), 1, "no second phase");
    }

    /// SPL token account (165 B) with the balance at offset 64 — like a real vault.
    fn vault(amount: u64) -> Vec<u8> {
        let mut a = vec![0u8; 165];
        a[64..72].copy_from_slice(&amount.to_le_bytes());
        a
    }

    /// Minimal PoolBuilder: two CP pools (base, T) from vault balances in the store (like CPMM/Pump).
    struct VaultBuilder;
    impl crate::run::PoolBuilder for VaultBuilder {
        fn account_keys(&self) -> Vec<Pubkey> {
            vec![id(20), id(21), id(30), id(31)]
        }
        fn build_pools(&self, src: &dyn AccountSource) -> Vec<engine::Pool> {
            let bal = |k: Pubkey| pools::parse_token_amount(src.get_account(&k).unwrap_or(&[]));
            let mut out = Vec::new();
            if let (Some(a), Some(b)) = (bal(id(20)), bal(id(21))) {
                out.push(engine::Pool {
                    id: id(10),
                    token_a: id(1),
                    token_b: id(2),
                    kind: engine::Kind::Cp { res_a: a, res_b: b, fee_bps: 30 },
                });
            }
            if let (Some(a), Some(b)) = (bal(id(30)), bal(id(31))) {
                out.push(engine::Pool {
                    id: id(11),
                    token_a: id(1),
                    token_b: id(2),
                    kind: engine::Kind::Cp { res_a: a, res_b: b, fee_bps: 30 },
                });
            }
            out
        }
    }

    #[test]
    fn live_feed_drives_pool_builder_into_engine() {
        use crate::run::PoolBuilder;
        // The live source returns 4 vaults in one slot: pool A base1e6/T2e6 (T cheap), B the opposite.
        let feed_src = MockFeed {
            batches: vec![vec![
                AccountUpdate { pubkey: id(20), data: vault(1_000_000), slot: 900 },
                AccountUpdate { pubkey: id(21), data: vault(2_000_000), slot: 900 },
                AccountUpdate { pubkey: id(30), data: vault(2_000_000), slot: 900 },
                AccountUpdate { pubkey: id(31), data: vault(1_000_000), slot: 900 },
            ]],
            calls: vec![],
        };
        let mut lf = LiveFeed::new(feed_src);
        let builder = VaultBuilder;

        // feed → store
        let n = lf.refresh_keys(&builder.account_keys()).unwrap();
        assert_eq!(n, 4);
        assert!(lf.store.is_consistent(&builder.account_keys(), 0), "one slot = consistent snapshot");

        // store (AccountSource) → build_pools → the engine finds a 2-leg opportunity
        let decoded = builder.build_pools(&lf.store);
        assert_eq!(decoded.len(), 2);
        let mut e = engine::Engine::new(id(1), 500_000, 0);
        for p in decoded {
            e.add_pool(p);
        }
        let ops = e.find_two_hop();
        assert!(!ops.is_empty(), "live feed → PoolBuilder → engine → opportunity");
        assert_eq!(ops[0].buy_pool, id(10), "buy T where it is cheap (pool A)");
        assert!(ops[0].profit > 0);
    }
}
