//! Manifest book. The layout is `MarketFixed` (256 B) plus 80 B blocks.
//!
//! Price is a `u128` quoting quote per 1 base, times `10^18`. Base input
//! walks down the bids, quote input down the asks. Rounding is the same as in
//! `impact_quote_atoms` / `impact_base_atoms`: fully taking an order rounds
//! toward the taker, a partial take the other way. A global order (`order_type` 3)
//! ends the walk, because without the global account balance the program goes no deeper.
//! An expired order (`last_valid_slot` nonzero and less than the slot) is
//! skipped. Slot 0 expires nothing.

const NIL: u32 = u32::MAX;
const HEADER: usize = 256;
const BLOCK: usize = 80;
const DISCRIMINANT: u64 = 4_859_840_929_024_028_656;
const D18: u128 = 1_000_000_000_000_000_000;
const MAX_LEVELS: usize = 128;
const ORDER_GLOBAL: u8 = 3;
const ORDER_MAX: u8 = 5;

/// A single resting order. `price` is in Manifest units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub price: u128,
    pub base: u64,
}

/// Bids and asks already ordered from the best price. Empty vector when that side has no orders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
}

/// Reads the mints and both sides of the book. `None` when the header is not a Manifest market.
pub fn decode(data: &[u8], slot: u32) -> Option<([u8; 32], [u8; 32], Book)> {
    if data.len() < HEADER {
        return None;
    }
    if u64::from_le_bytes(data[0..8].try_into().ok()?) != DISCRIMINANT {
        return None;
    }
    let mut base = [0u8; 32];
    let mut quote = [0u8; 32];
    base.copy_from_slice(&data[16..48]);
    quote.copy_from_slice(&data[48..80]);
    let bids_root = u32_at(data, 156)?;
    let bids_best = u32_at(data, 160)?;
    let asks_root = u32_at(data, 164)?;
    let asks_best = u32_at(data, 168)?;
    let dynamic = &data[HEADER..];
    Some((
        base,
        quote,
        Book {
            bids: walk(dynamic, bids_root, bids_best, slot, true),
            asks: walk(dynamic, asks_root, asks_best, slot, false),
        },
    ))
}

/// Sell base into the bids. Returns quote or `None` when nothing fills.
pub fn sell_base(levels: &mut Vec<Level>, base_in: u64) -> Option<u64> {
    let (out, used) = fill_bids(levels, base_in)?;
    consume(levels, used);
    Some(out)
}

/// Buy base with quote. Returns base or `None` when nothing fills.
/// Removes as many base atoms as the fill computed, without a second rounding.
pub fn buy_base(levels: &mut Vec<Level>, quote_in: u64) -> Option<u64> {
    let (out, used_base) = fill_asks(levels, quote_in)?;
    consume(levels, used_base);
    Some(out)
}

/// Quote needed to take the whole side. A full order rounds up.
pub fn quote_depth(levels: &[Level]) -> u64 {
    let mut total: u128 = 0;
    for level in levels {
        let Some(quote) = quote_for_base(level.price, level.base, true) else {
            return u64::MAX;
        };
        total = total.saturating_add(quote as u128);
    }
    u64::try_from(total).unwrap_or(u64::MAX)
}

/// Vaults from the market header. `None` when the account is not a Manifest market.
pub fn vaults(data: &[u8]) -> Option<([u8; 32], [u8; 32])> {
    if data.len() < HEADER {
        return None;
    }
    if u64::from_le_bytes(data.get(0..8)?.try_into().ok()?) != DISCRIMINANT {
        return None;
    }
    let mut base_vault = [0u8; 32];
    let mut quote_vault = [0u8; 32];
    base_vault.copy_from_slice(data.get(80..112)?);
    quote_vault.copy_from_slice(data.get(112..144)?);
    Some((base_vault, quote_vault))
}

fn fill_bids(levels: &[Level], base_in: u64) -> Option<(u64, u64)> {
    if base_in == 0 {
        return None;
    }
    let mut remaining = base_in;
    let mut total: u128 = 0;
    let mut used = 0u64;
    for level in levels {
        if remaining == 0 || level.base == 0 || level.price == 0 {
            break;
        }
        let matched = level.base.min(remaining);
        let full = remaining >= level.base;
        let quote = quote_for_base(level.price, matched, full)?;
        total = total.checked_add(quote as u128)?;
        used = used.checked_add(matched)?;
        if !full {
            break;
        }
        remaining -= matched;
    }
    let out = u64::try_from(total).ok()?;
    if out == 0 {
        None
    } else {
        Some((out, used))
    }
}

fn fill_asks(levels: &[Level], quote_in: u64) -> Option<(u64, u64)> {
    if quote_in == 0 {
        return None;
    }
    let mut remaining = quote_in;
    let mut total: u128 = 0;
    let mut used_base = 0u64;
    for level in levels {
        if remaining == 0 || level.base == 0 || level.price == 0 {
            break;
        }
        let limit = base_for_quote(level.price, remaining, false)?;
        if limit == 0 {
            break;
        }
        let matched = level.base.min(limit);
        let full = limit >= level.base;
        let quote = quote_for_base(level.price, matched, !full)?;
        if quote > remaining {
            break;
        }
        total = total.checked_add(matched as u128)?;
        used_base = used_base.checked_add(matched)?;
        if !full {
            break;
        }
        remaining -= quote;
    }
    let out = u64::try_from(total).ok()?;
    if out == 0 {
        None
    } else {
        Some((out, used_base))
    }
}

fn consume(levels: &mut Vec<Level>, base_used: u64) {
    let mut left = base_used;
    while left > 0 && !levels.is_empty() {
        if levels[0].base <= left {
            left -= levels[0].base;
            levels.remove(0);
        } else {
            levels[0].base -= left;
            break;
        }
    }
}

fn quote_for_base(price: u128, base: u64, round_up: bool) -> Option<u64> {
    let product = price.checked_mul(base as u128)?;
    let mut quote = product / D18;
    if round_up && product % D18 != 0 {
        quote = quote.checked_add(1)?;
    }
    u64::try_from(quote).ok()
}

fn base_for_quote(price: u128, quote: u64, round_up: bool) -> Option<u64> {
    if price == 0 {
        return Some(0);
    }
    let dividend = D18.checked_mul(quote as u128)?;
    let base = if round_up {
        dividend.div_ceil(price)
    } else {
        dividend / price
    };
    u64::try_from(base).ok()
}

fn walk(dynamic: &[u8], root: u32, mut index: u32, slot: u32, want_bid: bool) -> Vec<Level> {
    let mut out = Vec::new();
    let mut seen = 0u32;
    while index != NIL && out.len() < MAX_LEVELS && seen < 10_000 {
        seen += 1;
        let Some(node) = read_node(dynamic, index) else { break };
        if node.order_type == ORDER_GLOBAL || node.order_type > ORDER_MAX || node.is_bid != want_bid {
            break;
        }
        let expired = node.last_valid != 0 && node.last_valid < slot;
        if !expired && node.base > 0 && node.price > 0 {
            out.push(Level { price: node.price, base: node.base });
        }
        index = next_lower(dynamic, root, index, &node);
    }
    out
}

struct Node {
    left: u32,
    right: u32,
    parent: u32,
    price: u128,
    base: u64,
    last_valid: u32,
    is_bid: bool,
    order_type: u8,
}

fn read_node(dynamic: &[u8], index: u32) -> Option<Node> {
    let i = index as usize;
    let block = dynamic.get(i..i + BLOCK)?;
    Some(Node {
        left: u32::from_le_bytes(block[0..4].try_into().ok()?),
        right: u32::from_le_bytes(block[4..8].try_into().ok()?),
        parent: u32::from_le_bytes(block[8..12].try_into().ok()?),
        price: u128::from_le_bytes(block[16..32].try_into().ok()?),
        base: u64::from_le_bytes(block[32..40].try_into().ok()?),
        last_valid: u32::from_le_bytes(block[52..56].try_into().ok()?),
        is_bid: block[56] == 1,
        order_type: block[57],
    })
}

fn next_lower(dynamic: &[u8], root: u32, index: u32, node: &Node) -> u32 {
    if node.left != NIL {
        let mut current = node.left;
        for _ in 0..64 {
            let Some(child) = read_node(dynamic, current) else { return NIL };
            if child.right == NIL {
                return current;
            }
            current = child.right;
        }
        return NIL;
    }
    let mut current = index;
    let mut parent = node.parent;
    while current != root {
        let Some(up) = read_node(dynamic, parent) else { return NIL };
        if up.left != current {
            return parent;
        }
        current = parent;
        parent = up.parent;
    }
    NIL
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_node(buf: &mut [u8], index: u32, price: u128, base: u64, is_bid: bool, order_type: u8) {
        let i = HEADER + index as usize;
        buf[i..i + 12].fill(0xff);
        buf[i + 16..i + 32].copy_from_slice(&price.to_le_bytes());
        buf[i + 32..i + 40].copy_from_slice(&base.to_le_bytes());
        buf[i + 56] = u8::from(is_bid);
        buf[i + 57] = order_type;
    }

    fn market(bid_base: u64, ask_base: u64) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER + 160];
        buf[0..8].copy_from_slice(&DISCRIMINANT.to_le_bytes());
        buf[16] = 1;
        buf[48] = 2;
        buf[156..160].copy_from_slice(&0u32.to_le_bytes());
        buf[160..164].copy_from_slice(&0u32.to_le_bytes());
        buf[164..168].copy_from_slice(&80u32.to_le_bytes());
        buf[168..172].copy_from_slice(&80u32.to_le_bytes());
        put_node(&mut buf, 0, D18, bid_base, true, 0);
        put_node(&mut buf, 80, D18, ask_base, false, 0);
        buf
    }

    #[test]
    fn full_bid_rounds_up_and_a_partial_does_not() {
        let (_, _, mut book) = decode(&market(1, 1), 0).unwrap();
        book.bids[0].price = D18 + D18 / 2;
        assert_eq!(sell_base(&mut book.bids.clone(), 1), Some(2));
        book.bids[0].base = 2;
        assert_eq!(sell_base(&mut book.bids, 1), Some(1));
    }

    #[test]
    fn asks_pay_quote_for_base_one_to_one() {
        let (_, _, mut book) = decode(&market(10, 50), 0).unwrap();
        assert_eq!(book.bids[0].base, 10);
        assert_eq!(buy_base(&mut book.asks, 20), Some(20));
        assert_eq!(book.asks[0].base, 30);
    }

    #[test]
    fn a_global_order_stops_the_walk() {
        let mut buf = market(5, 5);
        put_node(&mut buf, 0, D18, 5, true, ORDER_GLOBAL);
        let (_, _, book) = decode(&buf, 0).unwrap();
        assert!(book.bids.is_empty());
        assert_eq!(book.asks.len(), 1);
    }

    #[test]
    fn partial_ask_consumes_the_matched_base() {
        // Fully taking 1 base at 1.5 pays 1 quote (rounded down). The remaining 5 quote
        // takes 5 from the next ask 1:1. 5 is left, not 6 from a second rounding.
        let (_, _, mut book) = decode(&market(1, 1), 0).unwrap();
        book.asks = vec![
            Level { price: D18 + D18 / 2, base: 1 },
            Level { price: D18, base: 10 },
        ];
        assert_eq!(buy_base(&mut book.asks, 6), Some(6));
        assert_eq!(book.asks, vec![Level { price: D18, base: 5 }]);
        assert_eq!(quote_depth(&[Level { price: D18 + D18 / 2, base: 1 }]), 2);
    }
}
