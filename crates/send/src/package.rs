//! Package to sign — the hand-off boundary to the operator.
//!
//! [`SignablePackage`] is a complete, explicit object: **message bytes to sign**, the public
//! key that must provide the signature, the opportunity economics (profit, cost, tip) and the suggested
//! submission channel. It deliberately **contains no private key and never touches the network** — signing (with the hot
//! bot key) and submission (TPU/SWQoS or a consent-based auction) are the operator's step, outside this crate.
//!
//! Thanks to this, all logic up to this point (feed → engine → route → budget → message) is pure
//! and testable without a network, and the only things added on the operator side are the signature and submit.

use crate::tx::Pubkey;

/// Submission channel suggested by the policy. Both are explicit/consent-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Public TPU/SWQoS (current/next leader). No auction tip.
    PublicTpu,
    /// Consent-based auction (Jito, BAM, Harmonic, Helius). The bid is a tip or a priority
    /// fee. This variant does not send a bundle.
    Auction,
}

/// Whether the message was built in durable nonce mode or on a fresh blockhash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blockhash {
    /// The `recent_blockhash` field is a fresh blockhash (window of ~150 blocks).
    Recent,
    /// The `recent_blockhash` field is the durable nonce value (tx does not expire; first ix = AdvanceNonce).
    DurableNonce,
}

/// Ready package to be signed and sent by the operator.
#[derive(Clone, Debug)]
pub struct SignablePackage {
    /// Message bytes (legacy or v0) **to be signed** with the bot key. These are exactly the bytes
    /// that go under ed25519 (the operator computes the signature and assembles the transaction: signatures ‖ message).
    pub message: Vec<u8>,
    /// **Public** key that must provide the (only) signature = bot key. No private key.
    pub fee_payer: Pubkey,
    /// Blockhash mode (fresh vs durable nonce).
    pub blockhash_mode: Blockhash,
    /// Expected gross profit of the opportunity (base lamports), from the engine valuation.
    pub expected_gross: u64,
    /// Fixed transaction cost (base + priority fee).
    pub fixed_cost: u64,
    /// Auction tip (0 for the public channel).
    pub tip: u64,
    /// Suggested submission channel.
    pub channel: Channel,
    /// Compute unit limit used in the message (for logs / correction).
    pub cu_limit: u32,
    /// Compute unit price (µlamport/CU) used in the message.
    pub cu_price_micro: u64,
}

impl SignablePackage {
    /// Net margin = gross profit − fixed cost − tip (0 if negative).
    pub fn net_margin(&self) -> u64 {
        self.expected_gross.saturating_sub(self.fixed_cost).saturating_sub(self.tip)
    }

    /// Whether sending pays off: net margin must be positive and ≥ `min_margin`.
    pub fn worth_sending(&self, min_margin: u64) -> bool {
        self.net_margin() >= min_margin && self.net_margin() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(gross: u64, cost: u64, tip: u64, channel: Channel) -> SignablePackage {
        SignablePackage {
            message: vec![1, 2, 3],
            fee_payer: [7u8; 32],
            blockhash_mode: Blockhash::DurableNonce,
            expected_gross: gross,
            fixed_cost: cost,
            tip,
            channel,
            cu_limit: 127_000,
            cu_price_micro: 61_825,
        }
    }

    #[test]
    fn net_margin_and_worth_sending() {
        let p = pkg(100_000, 8_000, 34_100, Channel::Auction);
        assert_eq!(p.net_margin(), 100_000 - 8_000 - 34_100);
        assert!(p.worth_sending(1));
        assert!(p.worth_sending(57_900));
        assert!(!p.worth_sending(57_901), "threshold higher than margin → do not send");
    }

    #[test]
    fn negative_margin_never_worth_sending() {
        let p = pkg(5_000, 8_000, 0, Channel::PublicTpu);
        assert_eq!(p.net_margin(), 0, "negative margin clamped to 0");
        assert!(!p.worth_sending(0));
    }
}
