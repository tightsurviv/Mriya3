//! Body of the stream that the private orderflow relayer pushes to our Block Engine.
//!
//! The schema is the same as `BlockEngineRelayer.StartExpiringPacketStream` in
//! `jito-labs/mev-protos`: one `PacketBatchUpdate` message is either an
//! `ExpiringPacketBatch` (TPU packets plus `expiry_ms`) or a `Heartbeat`.
//! We truncate `Packet.data` to `meta.size`. The vote and `discard` flags stay on the packet,
//! and [`crate::pending::scan`] and the runner skip them.
//!
//! This module does not listen on gRPC and does not pull in `tonic`.

use crate::pending::{self, Hit, Packet};

/// One packet after truncating `data` to `meta.size`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedPacket {
    pub bytes: Vec<u8>,
    pub discard: bool,
    pub simple_vote: bool,
}

impl OwnedPacket {
    pub fn as_packet(&self) -> Packet<'_> {
        Packet { bytes: &self.bytes, discard: self.discard, simple_vote: self.simple_vote }
    }
}

/// One message from the relayer stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayerUpdate {
    Heartbeat { count: u64 },
    Batch { expiry_ms: u32, packets: Vec<OwnedPacket> },
}

/// Decodes the `PacketBatchUpdate` body. Empty or truncated bytes give `None`.
///
/// When both `oneof` arms are in one buffer, the last one stays, as in proto3.
/// A packet with `meta.size` larger than `data` drops out of the batch. The rest of the batch stays.
pub fn decode_packet_batch_update(buf: &[u8]) -> Option<RelayerUpdate> {
    let mut cur = Cursor::new(buf);
    let mut seen: Option<RelayerUpdate> = None;
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 2) => {
                let raw = cur.bytes()?;
                let (expiry_ms, packets) = parse_expiring_batch(raw)?;
                seen = Some(RelayerUpdate::Batch { expiry_ms, packets });
            }
            (2, 2) => {
                let raw = cur.bytes()?;
                seen = Some(RelayerUpdate::Heartbeat { count: parse_heartbeat(raw)? });
            }
            _ => cur.skip(wire)?,
        }
    }
    seen
}

/// Like [`decode_packet_batch_update`], then [`pending::scan`] on the truncated packets.
/// A heartbeat gives an empty list.
pub fn scan_packet_batch_update(buf: &[u8], pools: &[[u8; 32]], programs: &[[u8; 32]]) -> Option<Vec<Hit>> {
    match decode_packet_batch_update(buf)? {
        RelayerUpdate::Heartbeat { .. } => Some(Vec::new()),
        RelayerUpdate::Batch { packets, .. } => {
            Some(pending::scan(packets.iter().map(OwnedPacket::as_packet), pools, programs))
        }
    }
}

fn parse_expiring_batch(buf: &[u8]) -> Option<(u32, Vec<OwnedPacket>)> {
    let mut cur = Cursor::new(buf);
    let mut expiry_ms = 0u32;
    let mut packets = Vec::new();
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (2, 2) => packets = parse_packet_batch(cur.bytes()?)?,
            (3, 0) => {
                let n = cur.varint()?;
                expiry_ms = u32::try_from(n).ok()?;
            }
            _ => cur.skip(wire)?,
        }
    }
    Some((expiry_ms, packets))
}

fn parse_packet_batch(buf: &[u8]) -> Option<Vec<OwnedPacket>> {
    let mut cur = Cursor::new(buf);
    let mut packets = Vec::new();
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 2) => {
                if let Some(packet) = parse_packet(cur.bytes()?)? {
                    packets.push(packet);
                }
            }
            _ => cur.skip(wire)?,
        }
    }
    Some(packets)
}

/// `None` in the outer `Option` = bad frame. `Some(None)` = packet to skip.
fn parse_packet(buf: &[u8]) -> Option<Option<OwnedPacket>> {
    let mut cur = Cursor::new(buf);
    let mut data = Vec::new();
    let mut size = 0u64;
    let mut discard = false;
    let mut simple_vote = false;
    let mut saw_meta = false;
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 2) => data = cur.bytes()?.to_vec(),
            (2, 2) => {
                let meta = cur.bytes()?;
                let parsed = parse_meta(meta)?;
                size = parsed.0;
                discard = parsed.1;
                simple_vote = parsed.2;
                saw_meta = true;
            }
            _ => cur.skip(wire)?,
        }
    }
    if !saw_meta {
        return Some(None);
    }
    let size = usize::try_from(size).ok()?;
    if size > data.len() {
        return Some(None);
    }
    Some(Some(OwnedPacket { bytes: data[..size].to_vec(), discard, simple_vote }))
}

fn parse_meta(buf: &[u8]) -> Option<(u64, bool, bool)> {
    let mut cur = Cursor::new(buf);
    let mut size = 0u64;
    let mut discard = false;
    let mut simple_vote = false;
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 0) => size = cur.varint()?,
            (4, 2) => {
                let flags = parse_flags(cur.bytes()?)?;
                discard = flags.0;
                simple_vote = flags.1;
            }
            _ => cur.skip(wire)?,
        }
    }
    Some((size, discard, simple_vote))
}

fn parse_flags(buf: &[u8]) -> Option<(bool, bool)> {
    let mut cur = Cursor::new(buf);
    let mut discard = false;
    let mut simple_vote = false;
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 0) => discard = cur.varint()? != 0,
            (4, 0) => simple_vote = cur.varint()? != 0,
            _ => cur.skip(wire)?,
        }
    }
    Some((discard, simple_vote))
}

fn parse_heartbeat(buf: &[u8]) -> Option<u64> {
    let mut cur = Cursor::new(buf);
    let mut count = 0u64;
    while !cur.done() {
        let (field, wire) = cur.key()?;
        match (field, wire) {
            (1, 0) => count = cur.varint()?,
            _ => cur.skip(wire)?,
        }
    }
    Some(count)
}

struct Cursor<'a> {
    buf: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, i: 0 }
    }

    fn done(&self) -> bool {
        self.i == self.buf.len()
    }

    fn key(&mut self) -> Option<(u64, u8)> {
        let key = self.varint()?;
        let field = key >> 3;
        if field == 0 {
            return None;
        }
        Some((field, (key & 7) as u8))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut n = 0u64;
        let mut shift = 0;
        loop {
            if shift >= 64 {
                return None;
            }
            let b = *self.buf.get(self.i)?;
            self.i += 1;
            if shift == 63 && b > 1 {
                return None;
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(n);
            }
            shift += 7;
        }
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.varint()?).ok()?;
        let start = self.i;
        self.i = self.i.checked_add(n)?;
        self.buf.get(start..self.i)
    }

    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.advance(8)?,
            2 => {
                self.bytes()?;
            }
            5 => self.advance(4)?,
            _ => return None,
        }
        Some(())
    }

    fn advance(&mut self, n: usize) -> Option<()> {
        self.i = self.i.checked_add(n)?;
        if self.i > self.buf.len() {
            None
        } else {
            Some(())
        }
    }
}

#[cfg(test)]
pub(crate) fn encode_packet_batch_for_test(packets: &[TestPacket<'_>]) -> Vec<u8> {
    let mut batch = Vec::new();
    for packet in packets {
        let mut one = Vec::new();
        push_bytes(&mut one, 1, packet.data);
        push_bytes(&mut one, 2, &encode_meta(packet.size, packet.discard, packet.simple_vote, packet.addr));
        push_bytes(&mut batch, 1, &one);
    }
    let mut expiring = Vec::new();
    push_bytes(&mut expiring, 2, &batch);
    push_varint_field(&mut expiring, 3, 157);
    let mut update = Vec::new();
    push_bytes(&mut update, 1, &expiring);
    update
}

#[cfg(test)]
pub(crate) struct TestPacket<'a> {
    pub data: &'a [u8],
    pub size: u64,
    pub discard: bool,
    pub simple_vote: bool,
    pub addr: &'a str,
}

#[cfg(test)]
pub(crate) fn encode_heartbeat_for_test(count: u64) -> Vec<u8> {
    let mut heartbeat = Vec::new();
    push_varint_field(&mut heartbeat, 1, count);
    let mut update = Vec::new();
    push_bytes(&mut update, 2, &heartbeat);
    update
}

#[cfg(test)]
fn encode_meta(size: u64, discard: bool, simple_vote: bool, addr: &str) -> Vec<u8> {
    let mut meta = Vec::new();
    push_varint_field(&mut meta, 1, size);
    if !addr.is_empty() {
        push_bytes(&mut meta, 2, addr.as_bytes());
    }
    let mut flags = Vec::new();
    if discard {
        push_varint_field(&mut flags, 1, 1);
    }
    if simple_vote {
        push_varint_field(&mut flags, 4, 1);
    }
    if !flags.is_empty() {
        push_bytes(&mut meta, 4, &flags);
    }
    meta
}

#[cfg(test)]
fn push_varint(buf: &mut Vec<u8>, mut n: u64) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        buf.push(b);
        if n == 0 {
            break;
        }
    }
}

#[cfg(test)]
fn push_varint_field(buf: &mut Vec<u8>, field: u32, n: u64) {
    push_varint(buf, (u64::from(field) << 3) | 0);
    push_varint(buf, n);
}

#[cfg(test)]
fn push_bytes(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    push_varint(buf, (u64::from(field) << 3) | 2);
    push_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_swap() -> Vec<u8> {
        pending::cpmm_exact_in_legacy([1u8; 32], [2u8; 32], 400_000, 2)
    }

    #[test]
    fn batch_trims_to_meta_size_and_skips_a_short_buffer() {
        let raw = raw_swap();
        let mut padded = raw.clone();
        padded.extend_from_slice(&[0xAA, 0xBB]);
        let body = encode_packet_batch_for_test(&[
            TestPacket { data: &padded, size: raw.len() as u64, discard: false, simple_vote: false, addr: "203.0.113.62:11228" },
            TestPacket { data: &raw, size: raw.len() as u64 + 8, discard: false, simple_vote: false, addr: "" },
            TestPacket { data: &raw, size: raw.len() as u64, discard: true, simple_vote: false, addr: "" },
            TestPacket { data: &raw, size: raw.len() as u64, discard: false, simple_vote: true, addr: "" },
        ]);
        let update = decode_packet_batch_update(&body).expect("batch");
        let RelayerUpdate::Batch { expiry_ms, packets } = update else { panic!("expected batch") };
        assert_eq!(expiry_ms, 157);
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0].bytes, raw);
        assert!(!packets[0].discard && !packets[0].simple_vote);
        assert!(packets[1].discard);
        assert!(packets[2].simple_vote);

        let tx = pending::decode_transaction(&raw).unwrap();
        let hits = scan_packet_batch_update(&body, &tx.static_keys, &[]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].signature, tx.signature);
    }

    #[test]
    fn heartbeat_and_last_oneof_arm_win() {
        let beat = encode_heartbeat_for_test(4);
        assert_eq!(decode_packet_batch_update(&beat), Some(RelayerUpdate::Heartbeat { count: 4 }));
        assert!(scan_packet_batch_update(&beat, &[], &[]).unwrap().is_empty());

        let raw = raw_swap();
        let batch = encode_packet_batch_for_test(&[TestPacket {
            data: &raw,
            size: raw.len() as u64,
            discard: false,
            simple_vote: false,
            addr: "",
        }]);
        let mut both = beat.clone();
        both.extend_from_slice(&batch);
        assert!(matches!(decode_packet_batch_update(&both), Some(RelayerUpdate::Batch { .. })));
        let mut reversed = batch;
        reversed.extend_from_slice(&beat);
        assert_eq!(decode_packet_batch_update(&reversed), Some(RelayerUpdate::Heartbeat { count: 4 }));
    }

    #[test]
    fn truncated_and_empty_updates_are_none() {
        assert!(decode_packet_batch_update(&[]).is_none());
        assert!(decode_packet_batch_update(&[0x0a]).is_none());
        assert!(decode_packet_batch_update(&[0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_none());
    }
}
