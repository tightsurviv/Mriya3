//! Decoding pool account state into inputs for the [`sim`] simulator.
//!
//! For constant-product pools (Pump AMM, Raydium v4/CPMM) the "state" is the addresses of two
//! token vaults; reserves = the balances of those accounts. So the pool decoder extracts
//! the vault addresses and mints, and the data layer (`feed`) supplies the balances — which we can also
//! read from a raw token account ([`parse_token_amount`]).
//!
//! To start with: Pump AMM (verified on a real account). Meteora DLMM and Raydium CLMM
//! will follow by the same method (determining offsets on real accounts) once we hook up `feed`.

pub mod clmm;
pub mod dlmm;
pub mod whirlpool;

pub type Pubkey = [u8; 32];

/// Address of the wrapped SOL (WSOL) mint.
pub const WSOL: Pubkey = [
    6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26,
    235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1,
];

/// Reads the balance from a raw SPL token account (field `amount`, u64 LE at offset 64).
/// Works for Token and Token-2022 (extensions come after the base 165 B layout).
pub fn parse_token_amount(data: &[u8]) -> Option<u64> {
    if data.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(data[64..72].try_into().ok()?))
}

/// SPL mint supply. The `supply` field is u64 LE at offset 36. A mint account is 82 B.
pub fn parse_mint_supply(data: &[u8]) -> Option<u64> {
    if data.len() < 82 {
        return None;
    }
    Some(u64::from_le_bytes(data[36..44].try_into().ok()?))
}

/// Dynamic AMM reserve: `token_vault * pool_lp / lp_supply`. Division rounds down.
/// The raw token vault balance is not the reserve when the LP virtual price is not 1.
pub fn vault_share(token_amount: u64, lp_balance: u64, lp_supply: u64) -> Option<u64> {
    if lp_supply == 0 {
        return None;
    }
    let v = (token_amount as u128).checked_mul(lp_balance as u128)? / lp_supply as u128;
    u64::try_from(v).ok()
}

/// Pump AMM (PumpSwap) — a constant-product pool.
pub mod pump_amm {
    use super::Pubkey;
    use sim::CpPool;

    /// Pump AMM program ID:`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`.
    pub const PROGRAM_ID_B58: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

    /// 8-byte pool account discriminator (Anchor). Determined on a real account.
    pub const POOL_DISCRIMINATOR: [u8; 8] = [0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc];

    // Field offsets in the pool account (211 B). Verified on account
    // JEBqxuDB3isvH1wf2Dpa9Q9P6BWUsZSAJnNU6ksz4rpB.
    const OFF_BASE_MINT: usize = 43;
    const OFF_QUOTE_MINT: usize = 75;
    const OFF_BASE_VAULT: usize = 139;
    const OFF_QUOTE_VAULT: usize = 171;
    const OFF_LP_SUPPLY: usize = 203;
    const POOL_LEN: usize = 211;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PumpPool {
        pub base_mint: Pubkey,
        pub quote_mint: Pubkey,
        pub base_vault: Pubkey,
        pub quote_vault: Pubkey,
        pub lp_supply: u64,
    }

    fn read_pubkey(data: &[u8], off: usize) -> Pubkey {
        let mut k = [0u8; 32];
        k.copy_from_slice(&data[off..off + 32]);
        k
    }

    /// Decodes a pool account. Returns `None` if the length or the discriminator does not match.
    pub fn decode(data: &[u8]) -> Option<PumpPool> {
        if data.len() < POOL_LEN || data[..8] != POOL_DISCRIMINATOR {
            return None;
        }
        Some(PumpPool {
            base_mint: read_pubkey(data, OFF_BASE_MINT),
            quote_mint: read_pubkey(data, OFF_QUOTE_MINT),
            base_vault: read_pubkey(data, OFF_BASE_VAULT),
            quote_vault: read_pubkey(data, OFF_QUOTE_VAULT),
            lp_supply: u64::from_le_bytes(data[OFF_LP_SUPPLY..OFF_LP_SUPPLY + 8].try_into().unwrap()),
        })
    }

    impl PumpPool {
        /// Builds a [`CpPool`] in the given swap direction from reserves (vault balances).
        ///
        /// `quote_in = true`: you put in quote (e.g. WSOL), you get base.
        /// `fee_bps`: total pool fee (PumpSwap ≈ 0.30% = 25 bps LP + 5 bps protocol;
        /// confirm the exact split from the global config account — hence the parameter).
        pub fn to_cp(&self, base_reserve: u64, quote_reserve: u64, fee_bps: u32, quote_in: bool) -> CpPool {
            if quote_in {
                CpPool::new(quote_reserve, base_reserve, fee_bps)
            } else {
                CpPool::new(base_reserve, quote_reserve, fee_bps)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Pump AMM pool account (JEBqxuDB…), 211 B, fetched from mainnet.
    const POOL_HEX: &str = "f19a6d0411b16dbcff0000fef83cee47c9b6733335c3ee9da78a707f3a8a511741a942061cea1e36b1a80d0d1fe32881fbd3dea81322c065e5eda2e2086644fec2565a0b1a312790ed053f069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001781c9d0ab5de0ea1e4b3b01ffeb24c28dccf9148586b6e2fe658646379c662cd94957e57ff09d3cc80e94fdfbd6777df13b34996d12844658d73a7cb318407c7818212cd53a985e7d5753b6de574ca5e4a3fdea8132639eee34f8a5d7c5cabff6400000000000000";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn decode_real_pump_pool() {
        let data = hex(POOL_HEX);
        assert_eq!(data.len(), 211);
        let p = pump_amm::decode(&data).expect("should decode");
        // quote_mint is WSOL — confirms the offsets are aligned.
        assert_eq!(p.quote_mint, WSOL);
        assert_eq!(p.lp_supply, 100);
        // base_vault B11Z2HEop… and quote_vault 9iYdaRfe… (bytes from mainnet).
        assert_eq!(
            p.base_vault,
            [148, 149, 126, 87, 255, 9, 211, 204, 128, 233, 79, 223, 189, 103, 119, 223, 19, 179, 73, 150, 209, 40, 68, 101, 141, 115, 167, 203, 49, 132, 7, 199]
        );
        assert_eq!(
            p.quote_vault,
            [129, 130, 18, 205, 83, 169, 133, 231, 213, 117, 59, 109, 229, 116, 202, 94, 74, 63, 222, 168, 19, 38, 57, 238, 227, 79, 138, 93, 124, 92, 171, 255]
        );
    }

    #[test]
    fn rejects_wrong_data() {
        assert!(pump_amm::decode(&[0u8; 211]).is_none(), "wrong discriminator");
        assert!(pump_amm::decode(&[0u8; 10]).is_none(), "too short");
    }

    #[test]
    fn to_cp_direction() {
        let data = hex(POOL_HEX);
        let p = pump_amm::decode(&data).unwrap();
        // quote_in: reserve_in = quote (1000), reserve_out = base (2000)
        let cp = p.to_cp(2000, 1000, 30, true);
        assert_eq!(cp.reserve_in, 1000);
        assert_eq!(cp.reserve_out, 2000);
    }

    #[test]
    fn token_amount_parse() {
        let mut acct = vec![0u8; 165];
        acct[64..72].copy_from_slice(&1_234_567u64.to_le_bytes());
        assert_eq!(parse_token_amount(&acct), Some(1_234_567));
        assert_eq!(parse_token_amount(&[0u8; 8]), None);
    }
}
