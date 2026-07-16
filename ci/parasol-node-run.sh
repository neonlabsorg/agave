#!/bin/sh
solana-test-validator \
  --ticks-per-slot 16 \
  --ledger /opt/parasol/data \
  --rpc-port 8899 \
  --faucet-port 19900 \
  --gossip-port 18000 \
  --limit-ledger-size 1000000 \
  --log
