# Parasol Custom Metrics

This project exposes custom node metrics via:

- `GET /metrics`

Typical URL:

- `http://<RPC_HOST>:<RPC_PORT>/metrics`
- Example: `http://127.0.0.1:8899/metrics`

## Main Metrics

### Counters

- `tx_accepted_total`: transactions accepted into the node retry/acceptance pipeline (deterministic acceptance point).
- `tx_executed_total`: committed transactions with successful execution (`status=Ok`).
- `tx_failed_total`: committed failed transactions (`status=Err`) and failed outcomes in retry flow.
- `tx_dropped_total`: transactions dropped before execution (ingress/scheduler/retry-pool overflow paths).
- `tx_expired_total`: transactions dropped due to expiry (blockhash/nonce validity window).
- `confirmation_timeout_rate`: tracked signatures that did not reach confirmation within timeout.
- `account_lock_conflict_rate`: lock conflict occurrences (`AccountInUse`-like execution contention).
- `retry_due_to_account_in_use_rate`: retries caused by account lock contention.
- `fork_rate`: fork-failure signal from replay/heaviest-fork failure states.
- `duplicate_confirmed_blocks`: duplicate-confirmed slot/hash events observed by replay logic.

### Gauge

- `avg_locked_accounts_per_tx`: average locked-account footprint per transaction.

### Histograms

- `node_ingress_latency_seconds`: RPC ingress processing latency (`t1 -> t2` proxy).
- `mempool_acceptance_latency_seconds`: acceptance latency in send-transaction-service (`t2 -> t3`).
- `acknowledge_latency_seconds`: end-to-end node acknowledge latency (`t1 -> t3`).
- `decision_response_latency_seconds`: node decision response latency in current RPC path (`forwarded -> RPC return` proxy).
- `node_to_decision_response_latency_seconds`: request-to-response latency at node side (`t1 -> response point`).
- `time_to_processed_seconds`: signature lifecycle latency to processed commitment.
- `time_to_confirmed_seconds`: signature lifecycle latency to confirmed commitment.
- `time_to_finalized_seconds`: signature lifecycle latency to finalized commitment.
- `tx_execution_in_block_latency_seconds`: latency from acceptance to processed execution in block (`t3 -> t5`, exact `t3` when available).
- `block_production_latency_seconds`: latency from acceptance to produced/frozen block (`t3 -> t6`, exact `t3` when available).
- `state_update_notification_latency_seconds`: pubsub notification preparation/queue/send latency (`t6 -> t7` approximation).
- `blockhash_fetch_latency_seconds`: RPC latency for `getLatestBlockhash`.
- `blockhash_age_at_submit_slots`: age of submitted blockhash at send time (in slots).
- `blockhash_remaining_validity_slots`: remaining validity window of submitted blockhash (in slots).

## `tx_type` Label Mapping (Config / CLI)

`tx_type` can be derived by rules keyed by:

- `program_id`
- first 8 bytes of `instruction_data` (instruction discriminator)

For Anchor methods, discriminator is computed as:

- `sha256("global:<method_name>")[0..8]`

### CLI Examples

Use a config file:

```bash
agave-validator \
  --rpc-tx-type-map-config /path/to/tx_type_rules.yaml
```

Inline rules (repeatable):

```bash
agave-validator \
  --rpc-tx-type-map-rule <PROGRAM_ID>:place_order:place_order \
  --rpc-tx-type-map-rule <PROGRAM_ID>:42:cancel_order \
  --rpc-tx-type-map-rule <PROGRAM_ID>:0x0102030405060708:liquidation
```

Rule format:

- `PROGRAM_ID:METHOD_NAME|DISCRIMINATOR_HEX|DISCRIMINATOR_U64:TX_TYPE`

`DISCRIMINATOR_U64` is converted with `to_le_bytes()` and matched against `instruction_data[0..8]`.

### Config File Example (YAML)

```yaml
rules:
  - program_id: 9xQeWvG816bUx9EPf2zj8F8oQv5gZ6kR3X4Y5Z6a7b8c
    method_name: place_order
    tx_type: place_order

  - program_id: 9xQeWvG816bUx9EPf2zj8F8oQv5gZ6kR3X4Y5Z6a7b8c
    discriminator: 42
    tx_type: cancel_order

  - program_id: 9xQeWvG816bUx9EPf2zj8F8oQv5gZ6kR3X4Y5Z6a7b8c
    discriminator: 0x0102030405060708
    tx_type: liquidation
```
