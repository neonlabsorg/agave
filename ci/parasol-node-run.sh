#!/bin/sh
# F10 subaccounts require two v4 ABI features deactivated (space-separated
# pubkeys, one --deactivate-feature each):
#   ptr9umik… direct_account_pointers_in_program_input — mutually exclusive with
#     the subaccount slot region; active → runtime reserves 0 slots, parasol-dex
#     sol_load_subaccount fails with MaxAccountsExceeded.
#   EDGMC5…  syscall_parameter_address_restrictions — rejects the subaccount
#     AccountInfo pointers F10 places in the input region during CPI; active →
#     deposit fails with "Invalid pointer".
# (account_data_direct_mapping / virtual_address_space_adjustments must stay
# ACTIVE — deactivating them breaks the v4 VM memory-region layout.)
DEACTIVATE_FEATURES="${DEACTIVATE_FEATURES:-ptr9umikaeAS7ZBBp2fsfRhie16F1V2jCKA2y6gXNAK EDGMC5kxFxGk4ixsNkGt8bW7QL5hDMXnbwaZvYMwNfzF}"
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
