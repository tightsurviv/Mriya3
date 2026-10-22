#!/usr/bin/env bash
# Tier-3b: local validator with a CLONED real Meteora DAMM v2 pool (WSOL) + deploy + client.
# Environment variables as in tier3.sh (required: MAINNET_RPC_URL).
set -u
: "${MAINNET_RPC_URL:?set MAINNET_RPC_URL (mainnet RPC to clone accounts from)}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${SOLANA_BIN:+$SOLANA_BIN/}"
S="${BIN}solana"; SK="${BIN}solana-keygen"; TV="${BIN}solana-test-validator"
RPC="http://127.0.0.1:8899"
WORK="${WORK_DIR:-$HOME/.arb-tier3}"
PAYER="$WORK/payer.json"
PROG_KP="$WORK/arb-prog.json"
CLIENT="${TIER3_CLIENT:-$REPO/tier3-client/target/release/tier3-client}"
if [ -z "${ARB_SO:-}" ]; then
  for c in "$REPO/target-sbf/deploy/arb_program_sbf.so" "$REPO/crates/program-sbf/target/deploy/arb_program_sbf.so"; do
    if [ -f "$c" ]; then ARB_SO="$c"; break; fi
  done
fi
if [ ! -f "${ARB_SO:-}" ]; then
  echo "missing .so program; build it: cargo-build-sbf --arch v3 --manifest-path crates/program-sbf/Cargo.toml (or set ARB_SO)"
  exit 1
fi

# Real DAMM v2 pool (WSOL) from an observed mainnet transaction.
DAMM=cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
TOKEN2022=TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb
POOL=GtPrGitz1jtUpHEsm734PYmiURQxYVC4hUWSEfVxMhUb
VAULT_A=3Tc15ckbDfxw1vXV2XEQoAzaovBmqLohbZvVWAwLNTSY
VAULT_B=CLR29ThHgQyek3T5BNM5u3zihPc76xW8HHGXpqLqZ1Ht
MINT_A=At541hRZhK9LWKj9w2dqN2wCdpFyZgRa1Un2yG7xa43m
WSOL=So11111111111111111111111111111111111111112

pkill -f solana-test-validator 2>/dev/null
sleep 2
mkdir -p "$WORK"
rm -rf "$WORK/tv-ledger" "$WORK/tv.log"

nohup "$TV" --reset --ledger "$WORK/tv-ledger" --url "$MAINNET_RPC_URL" \
  --clone-upgradeable-program "$DAMM" \
  --clone-upgradeable-program "$TOKEN2022" \
  --clone "$POOL" --clone "$VAULT_A" --clone "$VAULT_B" \
  --clone "$MINT_A" --clone "$WSOL" \
  > "$WORK/tv.log" 2>&1 < /dev/null &
disown

echo "waiting for RPC..."
ok=0
for i in $(seq 1 45); do
  if "$S" -u "$RPC" slot >/dev/null 2>&1; then ok=1; break; fi
  sleep 2
done
if [ "$ok" != "1" ]; then echo "RPC did not come up; log:"; tail -8 "$WORK/tv.log"; exit 1; fi
echo "RPC ready, slot=$("$S" -u "$RPC" slot 2>&1)"

[ -f "$PAYER" ] || "$SK" new --no-bip39-passphrase --force -s -o "$PAYER" >/dev/null 2>&1
"$S" -u "$RPC" -k "$PAYER" airdrop 30 >/dev/null 2>&1

echo "=== mainnet clones ==="
echo "pool:    $("$S" -u "$RPC" account "$POOL" 2>&1 | grep -iE 'Owner|Length' | tr '\n' ' ')"
echo "DAMM:    $("$S" -u "$RPC" account "$DAMM" 2>&1 | grep -i Executable)"
echo "vault_a: $("$S" -u "$RPC" account "$VAULT_A" 2>&1 | grep -i Length)"
echo "vault_b: $("$S" -u "$RPC" account "$VAULT_B" 2>&1 | grep -i Length)"
echo "mint_a:  $("$S" -u "$RPC" account "$MINT_A" 2>&1 | grep -iE 'Owner|Length' | tr '\n' ' ')"

echo "=== deploy our program (SBPF v3, fixed key) ==="
[ -f "$PROG_KP" ] || "$SK" new --no-bip39-passphrase --force -s -o "$PROG_KP" >/dev/null 2>&1
PROGID=$("$SK" pubkey "$PROG_KP")
"$S" -u "$RPC" -k "$PAYER" program deploy --program-id "$PROG_KP" "$ARB_SO" 2>&1 | tail -2
echo "program_id: $PROGID"

echo "=== CLIENT: real DAMM v2 swap through our CPI ==="
"$CLIENT" "$PROGID" dammv2 2>&1
