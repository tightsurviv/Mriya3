//! Assembling the signed transaction and the signing seam — **security boundary**.
//!
//! This module **holds no private key and never touches the network**. The operator implements [`Signer`]
//! with their own key/SDK; we only assemble the wire transaction bytes (message + signatures). The real submission
//! (`sendTransaction`) is a separate operator step — it is NOT here. The dry-run variant (simulation via
//! RPC) is in [`feed`], also without a real submit.

use crate::package::SignablePackage;
use crate::tx::Pubkey;

/// Transaction assembly/signing errors.
#[derive(Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// Signer's public key ≠ the package's `fee_payer`.
    WrongSigner,
    /// Number of signatures provided ≠ number required by the message header.
    SignatureCount { required: u8, got: usize },
    /// The package requires >1 signature — assemble the transaction via [`assemble_transaction`] with all signatures.
    MultipleSignersRequired(u8),
}

/// compact-u16 (shortvec) — as in message serialization.
fn shortvec(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

/// Number of required signatures from the message header: legacy → byte [0]; v0 (prefix `0x80`) → byte [1].
pub fn required_signatures(message: &[u8]) -> u8 {
    if message.is_empty() {
        return 0;
    }
    if message[0] >= 0x80 {
        *message.get(1).unwrap_or(&0)
    } else {
        message[0]
    }
}

/// Assembles the wire transaction bytes: `shortvec(num_signatures) ‖ signatures(64 B) ‖ message`.
/// The number of signatures must match the message header.
pub fn assemble_transaction(signatures: &[[u8; 64]], message: &[u8]) -> Result<Vec<u8>, SubmitError> {
    let req = required_signatures(message);
    if signatures.len() != req as usize {
        return Err(SubmitError::SignatureCount { required: req, got: signatures.len() });
    }
    let mut out = Vec::with_capacity(1 + signatures.len() * 64 + message.len());
    shortvec(signatures.len(), &mut out);
    for s in signatures {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(message);
    Ok(out)
}

/// Signing seam — implemented by the **operator** with their own key/SDK. The core NEVER holds the
/// private key: it gets only the message bytes and expects a 64-byte ed25519 signature.
pub trait Signer {
    /// Signer's public key (must = the package's `fee_payer`).
    fn pubkey(&self) -> Pubkey;
    /// ed25519 signature over the message bytes (64 B). The operator computes it with their private key.
    fn sign_message(&self, message: &[u8]) -> [u8; 64];
}

/// Signs the package with one signature (our invariant: 1 signature = bot) and assembles the wire transaction.
///
/// Checks that `signer.pubkey() == pkg.fee_payer` (otherwise [`SubmitError::WrongSigner`]). For
/// multi-signature packages returns [`SubmitError::MultipleSignersRequired`] — then the operator collects
/// all signatures and assembles the tx via [`assemble_transaction`]. **Does not send.**
pub fn sign_package(signer: &dyn Signer, pkg: &SignablePackage) -> Result<Vec<u8>, SubmitError> {
    if signer.pubkey() != pkg.fee_payer {
        return Err(SubmitError::WrongSigner);
    }
    let req = required_signatures(&pkg.message);
    if req != 1 {
        return Err(SubmitError::MultipleSignersRequired(req));
    }
    let sig = signer.sign_message(&pkg.message);
    assemble_transaction(&[sig], &pkg.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{Blockhash, Channel, SignablePackage};

    struct MockSigner {
        pk: Pubkey,
        sig: [u8; 64],
    }
    impl Signer for MockSigner {
        fn pubkey(&self) -> Pubkey {
            self.pk
        }
        fn sign_message(&self, _message: &[u8]) -> [u8; 64] {
            self.sig
        }
    }

    fn pkg_with(message: Vec<u8>, fee_payer: Pubkey) -> SignablePackage {
        SignablePackage {
            message,
            fee_payer,
            blockhash_mode: Blockhash::DurableNonce,
            expected_gross: 100_000,
            fixed_cost: 8_000,
            tip: 34_100,
            channel: Channel::Auction,
            cu_limit: 127_000,
            cu_price_micro: 61_825,
        }
    }

    #[test]
    fn required_signatures_legacy_and_v0() {
        assert_eq!(required_signatures(&[1, 0, 2]), 1, "legacy: byte 0");
        assert_eq!(required_signatures(&[0x80, 2, 0, 1]), 2, "v0: byte 1 after the 0x80 prefix");
        assert_eq!(required_signatures(&[]), 0);
    }

    #[test]
    fn assembles_wire_transaction() {
        let message = vec![1u8, 0, 2, 9, 9]; // header: 1 signature
        let sig = [7u8; 64];
        let tx = assemble_transaction(&[sig], &message).unwrap();
        assert_eq!(tx[0], 1, "shortvec of the signature count = 1");
        assert_eq!(&tx[1..65], &sig, "signature right after the count");
        assert_eq!(&tx[65..], &message[..], "message at the end");
    }

    #[test]
    fn assemble_rejects_wrong_signature_count() {
        let message = vec![2u8, 0, 1]; // requires 2 signatures
        let err = assemble_transaction(&[[7u8; 64]], &message).unwrap_err();
        assert_eq!(err, SubmitError::SignatureCount { required: 2, got: 1 });
    }

    #[test]
    fn sign_package_happy_path() {
        let fee_payer = [3u8; 32];
        let pkg = pkg_with(vec![1u8, 0, 2, 5, 5, 5], fee_payer);
        let signer = MockSigner { pk: fee_payer, sig: [9u8; 64] };
        let tx = sign_package(&signer, &pkg).unwrap();
        assert_eq!(tx[0], 1);
        assert_eq!(&tx[1..65], &[9u8; 64]);
        assert_eq!(&tx[65..], &pkg.message[..]);
    }

    #[test]
    fn sign_package_rejects_wrong_signer() {
        let pkg = pkg_with(vec![1u8, 0, 2], [3u8; 32]);
        let signer = MockSigner { pk: [4u8; 32], sig: [0u8; 64] };
        assert_eq!(sign_package(&signer, &pkg), Err(SubmitError::WrongSigner));
    }

    #[test]
    fn sign_package_rejects_multisig() {
        // Message requiring 2 signatures → points to assemble_transaction with the full set.
        let fee_payer = [3u8; 32];
        let pkg = pkg_with(vec![2u8, 0, 1], fee_payer);
        let signer = MockSigner { pk: fee_payer, sig: [0u8; 64] };
        assert_eq!(sign_package(&signer, &pkg), Err(SubmitError::MultipleSignersRequired(2)));
    }
}
