#!/bin/sh
solana-test-validator \
  --ledger /opt/parasol/data \
  --rpc-port 8899 \
  --faucet-port 19900 \
  --gossip-port 18000 \
  --limit-ledger-size 100000 \
  --log
