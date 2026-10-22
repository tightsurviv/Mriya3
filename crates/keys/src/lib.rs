//! Off-chain derivation of Solana addresses: `find_program_address` (PDA), the ATA address
//! and `create_with_seed` (nonce pool).
//!
//! Pure Rust, no external dependencies (crates.io is unreachable here because of TLS
//! interception). Implements SHA-256 and the "point on the ed25519 curve" test exactly as the
//! Solana runtime does (`create_program_address` rejects hashes that are curve points). Verified
//! on the bot's real addresses: state PDA, native-bridge PDA, WSOL ATA.
//!
//! This is the off-chain layer (route derived accounts: ATA, oracle, bin_array, tick_array, PDA vaults).

pub type Pubkey = [u8; 32];

pub mod venue_pda;

// ---- SHA-256 (FIPS 180-4) ----
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub struct Sha256 {
        state: [u32; 8],
        buf: [u8; 64],
        buflen: usize,
        total: u64,
    }

    impl Sha256 {
        pub fn new() -> Self {
            Sha256 {
                state: [
                    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
                ],
                buf: [0u8; 64],
                buflen: 0,
                total: 0,
            }
        }

        pub fn update(&mut self, mut data: &[u8]) {
            self.total = self.total.wrapping_add(data.len() as u64);
            while !data.is_empty() {
                let n = core::cmp::min(64 - self.buflen, data.len());
                self.buf[self.buflen..self.buflen + n].copy_from_slice(&data[..n]);
                self.buflen += n;
                data = &data[n..];
                if self.buflen == 64 {
                    let block = self.buf;
                    self.compress(&block);
                    self.buflen = 0;
                }
            }
        }

        pub fn finalize(mut self) -> [u8; 32] {
            let bitlen = self.total.wrapping_mul(8);
            self.update(&[0x80]);
            while self.buflen != 56 {
                self.update(&[0x00]);
            }
            let lenbytes = bitlen.to_be_bytes();
            // update() increases total, but we no longer read total after this — OK.
            self.update(&lenbytes);
            let mut out = [0u8; 32];
            for (i, w) in self.state.iter().enumerate() {
                out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
            }
            out
        }

        fn compress(&mut self, block: &[u8; 64]) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut h = self.state;
            for i in 0..64 {
                let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
                let ch = (h[4] & h[5]) ^ ((!h[4]) & h[6]);
                let t1 = h[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
                let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
                let t2 = s0.wrapping_add(maj);
                h[7] = h[6];
                h[6] = h[5];
                h[5] = h[4];
                h[4] = h[3].wrapping_add(t1);
                h[3] = h[2];
                h[2] = h[1];
                h[1] = h[0];
                h[0] = t1.wrapping_add(t2);
            }
            for i in 0..8 {
                self.state[i] = self.state[i].wrapping_add(h[i]);
            }
        }
    }
}

// ---- field F_p, p = 2^255-19, 5×51-bit representation (like the dalek u64 backend) ----
mod field {
    const LOW_51: u64 = (1 << 51) - 1;
    /// EDWARDS_D = -121665/121666 mod p.
    const D: [u64; 5] = [929955233495203, 466365720129213, 1662059464998953, 2033849074728123, 1442794654840575];
    const ONE: [u64; 5] = [1, 0, 0, 0, 0];
    /// (p-5)/8 = 2^252 - 3, little-endian bytes — the root exponent.
    const EXP_P58: [u8; 32] = [
        253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 15,
    ];

    fn from_bytes(b: &[u8; 32]) -> [u64; 5] {
        let load8 = |i: usize| -> u64 { u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) };
        [
            load8(0) & LOW_51,
            (load8(6) >> 3) & LOW_51,
            (load8(12) >> 6) & LOW_51,
            (load8(19) >> 1) & LOW_51,
            (load8(24) >> 12) & LOW_51, // the 51-bit mask removes bit 255 (sign of x)
        ]
    }

    fn reduce(l: [u64; 5]) -> [u64; 5] {
        let c = [l[0] >> 51, l[1] >> 51, l[2] >> 51, l[3] >> 51, l[4] >> 51];
        [
            (l[0] & LOW_51) + c[4] * 19,
            (l[1] & LOW_51) + c[0],
            (l[2] & LOW_51) + c[1],
            (l[3] & LOW_51) + c[2],
            (l[4] & LOW_51) + c[3],
        ]
    }

    fn add(a: [u64; 5], b: [u64; 5]) -> [u64; 5] {
        reduce([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]])
    }

    fn sub(a: [u64; 5], b: [u64; 5]) -> [u64; 5] {
        // (a + 2p) - b; requires b reduced (b_i < 2p_i).
        reduce([
            a[0] + 0xFFFFFFFFFFFDA - b[0],
            a[1] + 0xFFFFFFFFFFFFE - b[1],
            a[2] + 0xFFFFFFFFFFFFE - b[2],
            a[3] + 0xFFFFFFFFFFFFE - b[3],
            a[4] + 0xFFFFFFFFFFFFE - b[4],
        ])
    }

    fn neg(a: [u64; 5]) -> [u64; 5] {
        sub(ONE_ZERO, a)
    }
    const ONE_ZERO: [u64; 5] = [0, 0, 0, 0, 0];

    fn mul(a: [u64; 5], b: [u64; 5]) -> [u64; 5] {
        let m = |x: u64, y: u64| -> u128 { (x as u128) * (y as u128) };
        let c0 = m(a[0], b[0]) + 19 * (m(a[1], b[4]) + m(a[2], b[3]) + m(a[3], b[2]) + m(a[4], b[1]));
        let c1 = m(a[0], b[1]) + m(a[1], b[0]) + 19 * (m(a[2], b[4]) + m(a[3], b[3]) + m(a[4], b[2]));
        let c2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + 19 * (m(a[3], b[4]) + m(a[4], b[3]));
        let c3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + 19 * m(a[4], b[4]);
        let c4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        let low = LOW_51 as u128;
        let r0 = (c0 & low) as u64;
        let c1 = c1 + (c0 >> 51);
        let r1 = (c1 & low) as u64;
        let c2 = c2 + (c1 >> 51);
        let r2 = (c2 & low) as u64;
        let c3 = c3 + (c2 >> 51);
        let r3 = (c3 & low) as u64;
        let c4 = c4 + (c3 >> 51);
        let r4 = (c4 & low) as u64;
        let carry = (c4 >> 51) as u64;
        let mut r0 = r0 + carry * 19;
        let r1 = r1 + (r0 >> 51);
        r0 &= LOW_51;
        [r0, r1, r2, r3, r4]
    }

    fn square(a: [u64; 5]) -> [u64; 5] {
        mul(a, a)
    }

    fn pow_p58(a: [u64; 5]) -> [u64; 5] {
        let mut r = ONE;
        for byte in EXP_P58.iter().rev() {
            for i in (0..8).rev() {
                r = square(r);
                if (byte >> i) & 1 == 1 {
                    r = mul(r, a);
                }
            }
        }
        r
    }

    fn to_bytes(fe: [u64; 5]) -> [u8; 32] {
        let mut l = reduce(fe);
        let mut q = (l[0] + 19) >> 51;
        q = (l[1] + q) >> 51;
        q = (l[2] + q) >> 51;
        q = (l[3] + q) >> 51;
        q = (l[4] + q) >> 51;
        l[0] += 19 * q;
        l[1] += l[0] >> 51;
        l[0] &= LOW_51;
        l[2] += l[1] >> 51;
        l[1] &= LOW_51;
        l[3] += l[2] >> 51;
        l[2] &= LOW_51;
        l[4] += l[3] >> 51;
        l[3] &= LOW_51;
        l[4] &= LOW_51;
        let (l0, l1, l2, l3, l4) = (l[0], l[1], l[2], l[3], l[4]);
        let mut s = [0u8; 32];
        s[0] = l0 as u8;
        s[1] = (l0 >> 8) as u8;
        s[2] = (l0 >> 16) as u8;
        s[3] = (l0 >> 24) as u8;
        s[4] = (l0 >> 32) as u8;
        s[5] = (l0 >> 40) as u8;
        s[6] = ((l0 >> 48) | (l1 << 3)) as u8;
        s[7] = (l1 >> 5) as u8;
        s[8] = (l1 >> 13) as u8;
        s[9] = (l1 >> 21) as u8;
        s[10] = (l1 >> 29) as u8;
        s[11] = (l1 >> 37) as u8;
        s[12] = ((l1 >> 45) | (l2 << 6)) as u8;
        s[13] = (l2 >> 2) as u8;
        s[14] = (l2 >> 10) as u8;
        s[15] = (l2 >> 18) as u8;
        s[16] = (l2 >> 26) as u8;
        s[17] = (l2 >> 34) as u8;
        s[18] = (l2 >> 42) as u8;
        s[19] = ((l2 >> 50) | (l3 << 1)) as u8;
        s[20] = (l3 >> 7) as u8;
        s[21] = (l3 >> 15) as u8;
        s[22] = (l3 >> 23) as u8;
        s[23] = (l3 >> 31) as u8;
        s[24] = (l3 >> 39) as u8;
        s[25] = ((l3 >> 47) | (l4 << 4)) as u8;
        s[26] = (l4 >> 4) as u8;
        s[27] = (l4 >> 12) as u8;
        s[28] = (l4 >> 20) as u8;
        s[29] = (l4 >> 28) as u8;
        s[30] = (l4 >> 36) as u8;
        s[31] = (l4 >> 44) as u8;
        s
    }

    fn eq(a: [u64; 5], b: [u64; 5]) -> bool {
        to_bytes(a) == to_bytes(b)
    }

    /// Whether 32 bytes encode a point on the ed25519 curve (like `bytes_are_curve_point` in Solana).
    pub fn is_on_curve(bytes: &[u8; 32]) -> bool {
        let y = from_bytes(bytes);
        let yy = square(y);
        let u = sub(yy, ONE);
        let v = add(mul(D, yy), ONE);
        let v2 = square(v);
        let v3 = mul(v2, v);
        let v3sq = square(v3);
        let v7 = mul(v3sq, v);
        let uv3 = mul(u, v3);
        let uv7 = mul(u, v7);
        let r = mul(uv3, pow_p58(uv7));
        let check = mul(v, square(r));
        eq(check, u) || eq(check, neg(u))
    }
}

use sha256::Sha256;

/// SHA-256 (FIPS 180-4) of a single buffer — helper (e.g. Anchor discriminators `sha256("global:<ix>")`).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Durable nonce domain prefix — exactly as in the Solana runtime (`DurableNonce::from_blockhash`
/// computes `hashv(&[b"DURABLE_NONCE", blockhash])`, and `hashv` is SHA-256).
pub const DURABLE_NONCE_PREFIX: &[u8] = b"DURABLE_NONCE";

/// Durable nonce value computed from the blockhash: `sha256("DURABLE_NONCE" ‖ blockhash)`.
///
/// This is the same value the runtime stores in the nonce account after the `AdvanceNonceAccount` instruction.
/// By computing it locally from the last observed blockhash, the bot can assemble a transaction in
/// durable nonce mode **without reading the nonce account over RPC** — the same optimization used
/// by the reference bot. The value goes into the message's `recent_blockhash` field.
pub fn durable_nonce(blockhash: &Pubkey) -> Pubkey {
    let mut h = Sha256::new();
    h.update(DURABLE_NONCE_PREFIX);
    h.update(blockhash);
    h.finalize()
}

/// System program `11111111111111111111111111111111`.
pub const SYSTEM_PROGRAM: Pubkey = [0u8; 32];

/// Longest `create_with_seed` seed (like `MAX_SEED_LEN` in the runtime).
pub const MAX_SEED_LEN: usize = 32;

/// Number of accounts in the pool `execution-n-0000` … `execution-n-0999`.
pub const EXECUTION_NONCE_COUNT: u16 = 1000;

/// `createAccountWithSeed` address: `sha256(base ‖ seed ‖ owner)`.
///
/// This is not a PDA: there is no `ProgramDerivedAddress` suffix and the result may lie on the curve.
/// A seed longer than 32 bytes returns `None`.
pub fn create_with_seed(base: &Pubkey, seed: &str, owner: &Pubkey) -> Option<Pubkey> {
    if seed.len() > MAX_SEED_LEN {
        return None;
    }
    let mut h = Sha256::new();
    h.update(base);
    h.update(seed.as_bytes());
    h.update(owner);
    Some(h.finalize())
}

/// Account `execution-n-{index:04}` derived from `base`, owner = system program.
///
/// The index is in the range `0..1000`. The nonce value stored in the account is still
/// [`durable_nonce`] of the previous blockhash — this function only gives the address.
pub fn execution_nonce_account(base: &Pubkey, index: u16) -> Option<Pubkey> {
    if index >= EXECUTION_NONCE_COUNT {
        return None;
    }
    create_with_seed(base, &format!("execution-n-{index:04}"), &SYSTEM_PROGRAM)
}

/// Pair `(2·pair, 2·pair+1)` from the pool of a thousand accounts. `pair` is in the range `0..500`.
///
/// The runner puts only the even account into the transaction. The role of the odd partner
/// is not described in the measurement, so it is derived and unused.
pub fn execution_nonce_pair(base: &Pubkey, pair: u16) -> Option<(Pubkey, Pubkey)> {
    if pair >= EXECUTION_NONCE_COUNT / 2 {
        return None;
    }
    let even = execution_nonce_account(base, pair * 2)?;
    let odd = execution_nonce_account(base, pair * 2 + 1)?;
    Some((even, odd))
}

/// Associated token account program (ATA) `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`.
pub const ATA_PROGRAM: Pubkey = [140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89];
/// SPL Token `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`.
pub const TOKEN_PROGRAM: Pubkey = [6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169];
/// SPL Token-2022 `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`.
pub const TOKEN_2022_PROGRAM: Pubkey = [6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77, 131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252];

const PDA_MARKER: &[u8] = b"ProgramDerivedAddress";

/// Whether 32 bytes lie on the ed25519 curve (an "ordinary" key); a PDA must be **off** the curve.
pub fn is_on_curve(bytes: &Pubkey) -> bool {
    field::is_on_curve(bytes)
}

/// `create_program_address`: hash(seeds ‖ program_id ‖ "ProgramDerivedAddress"); `None` if
/// the result is a curve point (then it is not a valid PDA).
pub fn create_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> Option<Pubkey> {
    let mut h = Sha256::new();
    for s in seeds {
        h.update(s);
    }
    h.update(program_id);
    h.update(PDA_MARKER);
    let d = h.finalize();
    if field::is_on_curve(&d) {
        None
    } else {
        Some(d)
    }
}

/// `find_program_address`: finds the highest `bump` (255→0) for which the address is off the curve.
pub fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    let mut bump = 255u8;
    loop {
        let bm = [bump];
        let mut v: Vec<&[u8]> = Vec::with_capacity(seeds.len() + 1);
        v.extend_from_slice(seeds);
        v.push(&bm);
        if let Some(pk) = create_program_address(&v, program_id) {
            return (pk, bump);
        }
        assert!(bump != 0, "no valid bump found");
        bump -= 1;
    }
}

/// ATA address: `find_program_address([wallet, token_program, mint], ATA_PROGRAM).0`.
pub fn associated_token_address(wallet: &Pubkey, token_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    find_program_address(&[wallet, token_program, mint], &ATA_PROGRAM).0
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF_WALLET: Pubkey = [5, 87, 214, 198, 98, 247, 219, 155, 23, 158, 96, 49, 199, 215, 83, 103, 73, 11, 6, 14, 71, 34, 167, 70, 233, 126, 82, 150, 103, 249, 236, 34];
    const WSOL: Pubkey = [6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26, 235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1];
    const REF_PROGRAM: Pubkey = [139, 27, 135, 252, 177, 229, 90, 244, 21, 200, 149, 175, 106, 172, 31, 167, 1, 136, 179, 62, 31, 95, 151, 28, 185, 108, 184, 82, 91, 241, 226, 106];
    const STATE_PDA: Pubkey = [57, 93, 101, 186, 85, 219, 100, 21, 148, 23, 7, 209, 39, 186, 135, 20, 202, 66, 82, 19, 101, 122, 144, 210, 209, 223, 200, 98, 108, 83, 216, 255];
    const NATIVE_BRIDGE_PDA: Pubkey = [194, 63, 241, 44, 247, 127, 43, 57, 64, 244, 233, 248, 25, 146, 246, 110, 235, 193, 115, 202, 173, 232, 204, 207, 8, 231, 88, 120, 62, 12, 194, 23];
    const BOT_WSOL_ATA: Pubkey = [112, 180, 225, 211, 245, 2, 246, 110, 79, 234, 238, 102, 177, 146, 153, 9, 163, 65, 87, 64, 124, 110, 70, 82, 126, 127, 239, 71, 64, 133, 60, 159];

    #[test]
    fn sha256_known_vectors() {
        // "" and "abc" — FIPS vectors.
        let empty = {
            let h = Sha256::new();
            h.finalize()
        };
        assert_eq!(
            empty,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
                0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55
            ]
        );
        let abc = {
            let mut h = Sha256::new();
            h.update(b"abc");
            h.finalize()
        };
        assert_eq!(
            abc,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
                0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    #[test]
    fn on_curve_distinguishes_wallet_from_pda() {
        // The wallet key lies on the curve; a PDA (by construction) lies off it.
        assert!(is_on_curve(&REF_WALLET), "the wallet key is a curve point");
        assert!(!is_on_curve(&STATE_PDA), "PDA is off the curve");
        assert!(!is_on_curve(&NATIVE_BRIDGE_PDA), "PDA is off the curve");
    }

    #[test]
    fn state_pda_matches_real() {
        let (pk, bump) = find_program_address(&[b"state"], &REF_PROGRAM);
        assert_eq!(pk, STATE_PDA, "state PDA = 4rvqi…");
        assert_eq!(bump, 253, "bump 253 (as on-chain)");
    }

    #[test]
    fn native_bridge_pda_matches_real() {
        let (pk, bump) = find_program_address(&[b"native-bridge", &REF_WALLET, &WSOL], &REF_PROGRAM);
        assert_eq!(pk, NATIVE_BRIDGE_PDA, "native-bridge PDA = E5GeMg2J…");
        assert_eq!(bump, 255);
    }

    #[test]
    fn ata_matches_real() {
        let ata = associated_token_address(&REF_WALLET, &TOKEN_PROGRAM, &WSOL);
        assert_eq!(ata, BOT_WSOL_ATA, "bot WSOL ATA = 8axdqvEo…");
    }

    #[test]
    fn durable_nonce_matches_reference() {
        // Vectors computed independently (hashlib): sha256("DURABLE_NONCE" ‖ blockhash).
        // Proves the prefix domain and the concatenation order relative to the Solana runtime.
        assert_eq!(
            durable_nonce(&[0u8; 32]),
            [122, 59, 213, 205, 242, 144, 9, 142, 91, 76, 143, 153, 87, 249, 137, 146, 16, 184, 187, 233, 6, 79, 114, 179, 38, 204, 33, 46, 171, 71, 111, 138]
        );
        let mut seq = [0u8; 32];
        for (i, b) in seq.iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(
            durable_nonce(&seq),
            [32, 180, 98, 12, 167, 64, 109, 237, 29, 238, 153, 28, 161, 52, 197, 199, 215, 180, 29, 110, 176, 43, 23, 101, 34, 184, 77, 102, 196, 249, 6, 143]
        );
        // Deterministic and sensitive to the blockhash.
        assert_ne!(durable_nonce(&[0u8; 32]), durable_nonce(&seq));
    }

    #[test]
    fn execution_nonce_pool_matches_published_addresses() {
        // sha256(REF_WALLET ‖ "execution-n-NNNN" ‖ system program). Independent hashlib.
        let n0 = [21, 134, 208, 209, 81, 95, 3, 66, 227, 57, 209, 12, 223, 66, 2, 239, 79, 118, 172, 77, 91, 255, 102, 22, 96, 198, 12, 192, 186, 126, 208, 32];
        let n999 = [113, 228, 173, 246, 79, 210, 119, 70, 68, 44, 137, 81, 155, 183, 199, 31, 141, 5, 242, 112, 190, 12, 136, 239, 214, 236, 145, 10, 34, 44, 93, 77];
        assert_eq!(execution_nonce_account(&REF_WALLET, 0), Some(n0));
        assert_eq!(execution_nonce_account(&REF_WALLET, 999), Some(n999));
        assert_eq!(execution_nonce_pair(&REF_WALLET, 0).unwrap().0, n0);
        assert_eq!(execution_nonce_pair(&REF_WALLET, 499).unwrap().1, n999);
        assert!(execution_nonce_account(&REF_WALLET, 1000).is_none());
        assert!(execution_nonce_pair(&REF_WALLET, 500).is_none());
        assert!(create_with_seed(&REF_WALLET, &"x".repeat(33), &SYSTEM_PROGRAM).is_none());
        let seed32 = "x".repeat(32);
        assert!(create_with_seed(&REF_WALLET, &seed32, &SYSTEM_PROGRAM).is_some());
    }
}
