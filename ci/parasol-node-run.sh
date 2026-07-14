#!/bin/sh
# SIMD-0449 (direct_account_pointers_in_program_input) is mutually exclusive with
# F10 subaccount slots: while it is active the runtime reserves 0 subaccount slots,
# so parasol-dex sol_load_subaccount fails with MaxAccountsExceeded. Deactivate it
# (space-separated pubkeys, one --deactivate-feature each).
DEACTIVATE_FEATURES="${DEACTIVATE_FEATURES:-ptr9umikaeAS7ZBBp2fsfRhie16F1V2jCKA2y6gXNAK}"
DEACTIVATE_FLAGS=""
for f in ${DEACTIVATE_FEATURES}; do
  [ -n "${f}" ] && DEACTIVATE_FLAGS="${DEACTIVATE_FLAGS} --deactivate-feature ${f}"
done

solana-test-validator \
  --ticks-per-slot 16 \
  --ledger /opt/parasol/data \
  --rpc-port 8899 \
  --faucet-port 19900 \
  --gossip-port 18000 \
  --limit-ledger-size 1000000 \
  ${DEACTIVATE_FLAGS} \
  --log
