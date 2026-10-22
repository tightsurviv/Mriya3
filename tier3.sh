#!/usr/bin/env bash
# Tier-3: local validator with a real CPMM pool (mainnet clone) + deploy of our program.
#
# Required:
#   MAINNET_RPC_URL  mainnet RPC the validator clones accounts from (e.g. a provider URL with an API key).
# Optional:
#   SOLANA_BIN       directory with solana / solana-keygen / solana-test-validator (default: PATH)
#   WORK_DIR         ledger, log and throwaway test keys (default: ~/.arb-tier3)
#   ARB_SO           program built with --arch v3 (default: target-sbf/deploy/arb_program_sbf.so
#                    or crates/program-sbf/target/deploy/arb_program_sbf.so)
#   TIER3_CLIENT     client binary (default: tier3-client/target/release/tier3-client)
# Keys created by the script are only for the local validator. The global Solana CLI config
# is not changed.
set -u
: "${MAINNET_RPC_URL:?set MAINNET_RPC_URL (mainnet RPC to clone accounts from)}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${SOLANA_BIN:+$SOLANA_BIN/}"
S="${BIN}solana"
SK="${BIN}solana-keygen"
TV="${BIN}solana-test-validator"
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

# CPMM pool accounts (WSOL) to clone.
POOL=7wTChQDj3KG4kHXYjWrPw5bWUkKzNpJySU5CABEkHogv
AMMCFG=D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2
OBS=AEUT5QncT8Gpg41qLpsXup3MY39LNrWwZYFXNSG5iq2V
VAULT_TOK=BjxbTebtrZu44CVfZ2GzDLKZrmyitFTkny1WqeWmmVxH
VAULT_WSOL=GycqBfycFFRcURh3aWVCpzcFSicsk23omeZPtdnguGvd
MINT_TOK=BzMWbt7ko3P8c457gzxuBCCt6q73sJyG98nQNeTfcCom
WSOL=So11111111111111111111111111111111111111112
CPMM=CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C
TOKEN2022=TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb

pkill -f solana-test-validator 2>/dev/null
sleep 2
mkdir -p "$WORK"
rm -rf "$WORK/tv-ledger" "$WORK/tv.log"

nohup "$TV" --reset --ledger "$WORK/tv-ledger" --url "$MAINNET_RPC_URL" \
  --clone-upgradeable-program "$CPMM" \
  --clone-upgradeable-program "$TOKEN2022" \
  --clone "$POOL" --clone "$AMMCFG" --clone "$OBS" \
  --clone "$VAULT_TOK" --clone "$VAULT_WSOL" \
  --clone "$MINT_TOK" --clone "$WSOL" \
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
echo "deployer balance: $("$S" -u "$RPC" -k "$PAYER" balance 2>&1)"

echo "=== mainnet clones ==="
echo "pool_state: $("$S" -u "$RPC" account "$POOL" 2>&1 | grep -iE 'Owner|Length' | tr '\n' ' ')"
echo "CPMM program: $("$S" -u "$RPC" account "$CPMM" 2>&1 | grep -i Executable)"
echo "amm_config: $("$S" -u "$RPC" account "$AMMCFG" 2>&1 | grep -i Length)"
echo "vault WSOL: $("$S" -u "$RPC" account "$VAULT_WSOL" 2>&1 | grep -i Length)"

echo "=== deploy our program (SBPF v3, fixed key) ==="
[ -f "$PROG_KP" ] || "$SK" new --no-bip39-passphrase --force -s -o "$PROG_KP" >/dev/null 2>&1
PROGID=$("$SK" pubkey "$PROG_KP")
"$S" -u "$RPC" -k "$PAYER" program deploy --program-id "$PROG_KP" "$ARB_SO" 2>&1 | tail -2
echo "program_id: $PROGID"

echo "=== CLIENT: real swap through our CPI into the cloned CPMM pool ==="
"$CLIENT" "$PROGID" 2>&1
