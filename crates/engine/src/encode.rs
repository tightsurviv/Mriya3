//! Route encoder: turns a route plan into instruction bytes for the on-chain program.
//!
//! This ties the engine to the program: the bytes produced here are parsed exactly by
//! `arb_program::parse_header` / `arb_program::hop`.

pub use arb_program::{profit_in_lamports, RouteMeta};

use arb_program::Hop;

/// One leg of the dispatch plan (venue = exchange code in the program, direction, number of CPI accounts).
/// `token_account_size` = 0 when the intermediate account is a persistent ATA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanHop {
    pub venue: u8,
    pub direction: u8,
    pub n_accounts: u8,
    pub token_account_size: u16,
}

/// Encodes route `0x0a`. `None` when the number of legs is outside `2..=6`.
pub fn encode_route(amount_in: u64, min_profit: u64, hops: &[PlanHop], meta: &RouteMeta) -> Option<Vec<u8>> {
    let mapped: Vec<Hop> = hops
        .iter()
        .map(|h| Hop {
            venue: h.venue,
            direction: h.direction,
            n_accounts: h.n_accounts,
            token_account_size: h.token_account_size,
        })
        .collect();
    arb_program::encode_route(amount_in, min_profit, &mapped, meta).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_program::{hop, parse_header};

    #[test]
    fn layout_matches_measured_0x0a() {
        let hops = [
            PlanHop { venue: 0x03, direction: 0, n_accounts: 15, token_account_size: 0 },
            PlanHop { venue: 0x01, direction: 1, n_accounts: 18, token_account_size: 165 },
        ];
        let data = encode_route(448_245_643, 0, &hops, &RouteMeta::new(1, 2_596_311)).unwrap();
        assert_eq!(data.len(), 107);
        assert_eq!(data[0], 0x0a);
        assert_eq!(&data[1..9], &448_245_643u64.to_le_bytes());
        assert_eq!(&data[9..17], &0u64.to_le_bytes());
        assert_eq!(data[62], 0x03);
        assert_eq!(u16::from_le_bytes([data[65], data[66]]), 0);
        assert_eq!(data[67], 0x01);
        assert_eq!(u16::from_le_bytes([data[70], data[71]]), 165);
        assert_eq!(data[72], 0);
        assert_eq!(data[73], 1);
        assert_eq!(&data[74..77], &[0, 0, 0]);
        assert_eq!(&data[77..81], b"MRXP");
        assert_eq!(data[81], 2);
        assert_eq!(data[82], 0);
        assert_eq!(i64::from_le_bytes(data[83..91].try_into().unwrap()), 2_596_311);
        assert_eq!(&data[91..99], &0u64.to_le_bytes());
        assert_eq!(&data[99..107], &[0u8; 8]);

        let h = parse_header(&data).unwrap();
        assert_eq!(h.n_hops, 2);
        assert_eq!(h.min_profit, 0);
        assert_eq!((hop(&data, 0).unwrap().venue, hop(&data, 1).unwrap().venue), (0x03, 0x01));

        assert!(encode_route(1, 0, &hops[..1], &RouteMeta::new(1, 0)).is_none());
        let seven = [hops[0]; 7];
        assert!(encode_route(1, 0, &seven, &RouteMeta::new(1, 0)).is_none());
    }
}
