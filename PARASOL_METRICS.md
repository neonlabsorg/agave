# Parasol Agave — Metrics Catalog

Inventory of every metric-emitting / state-tracking function in this fork
(`parasol-fork-dev-metrics` based on `parasol-fork-dev`).

Columns:
- **Datapoint** — name emitted to InfluxDB via `datapoint_info!`. State-only
  functions (no emit) are shown as `*function_name* (no emit)`.
- **file:line** — call site (where the metric is triggered). Multiple sites are
  listed as separate rows. Functions with no callers show their definition site
  with `(definition; never called)`.
- **Trigger** — what causes the call to happen.
- **Frequency** — observed / expected rate, plus any gating that limits it.

---

## 1. M1 custom_metrics — `metrics/src/custom_metrics.rs`

### 1.1 Acceptance-time HashMap (state-only, no datapoint emit)

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `register_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:341` | RPC `sendTransaction` accepted | Per accepted RPC tx |
| `get_tx_acceptance_time` (no emit) | `rpc/src/signature_metrics_tracker.rs:55` | Inside `effective_t3()` lookup | Per `mark_processed_with_slot` / `mark_block_produced` (TRACKER_ACTIVE-gated) |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:349` | `max_retries == Some(0)` early-exit | Per fire-and-forget RPC tx |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:365` | `retry_pool_full` early-exit | Rare (only when retry pool overflows) |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:520` | Tx confirmed in `process_transactions` | Per finalized retry-pool tx |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:534` | Tx blockhash expired | Per expired retry-pool tx |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:548` | Tx dropped after `max_retries` | Per max-retries-exceeded tx |
| `clear_tx_acceptance_time` (no emit) | `send-transaction-service/src/send_transaction_service.rs:596` | Service exit cleanup | Once per shutdown |
| `clear_tx_acceptance_time` (no emit) | `rpc/src/signature_metrics_tracker.rs:128` | `mark_finalized` removes signature | Per finalized tx (TRACKER_ACTIVE-gated) |
| `clear_tx_acceptance_time` (no emit) | `rpc/src/signature_metrics_tracker.rs:188` | Sweep removes timed-out entry | Every 5 s (sweeper-gated) |

### 1.2 TX type label registry

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `init_tx_type_series` (no-op in M1) | `rpc/src/tx_type_rules.rs:38` | Inside `set_rules()` after parsing rule list | Once at rule load (currently never — `set_rules` has no caller) |

### 1.3 TX lifecycle counters

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `tx_accepted_total` | `send-transaction-service/src/send_transaction_service.rs:326` | RPC `sendTransaction` accepted | Per accepted RPC tx |
| `tx_accepted_total` (with `tx_type` tag) | `send-transaction-service/src/send_transaction_service.rs:336` | Same, per inferred tx_type label | Per (accepted tx × matched type) tuple |
| `tx_executed_total` | `rpc/src/transaction_status_service.rs:191` | Tx committed successfully | Per successfully executed tx |
| `tx_executed_total` (with `tx_type` tag) | `rpc/src/transaction_status_service.rs:193` | Same, per type | Per (executed tx × type) |
| `tx_failed_total` | `rpc/src/transaction_status_service.rs:196` | Tx committed with error | Per failed tx |
| `tx_failed_total` (with `tx_type` tag) | `rpc/src/transaction_status_service.rs:198` | Same, per type | Per (failed tx × type) |
| `tx_dropped_total` | `send-transaction-service/src/send_transaction_service.rs:357` | Tx dropped because retry pool full | Per overflow event |
| `tx_dropped_total` | `send-transaction-service/src/send_transaction_service.rs:382` | Tx dropped due to retry-queue overflow accounting | Per accounting cycle |
| `tx_dropped_total` | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:423` | Scheduler rejected tx (sum across all reasons) | Per scheduler iteration when any drop occurred |
| `tx_dropped_total` (with `reason` tag) | `send-transaction-service/src/send_transaction_service.rs:358` | reason = "retry_pool_full" | Per overflow event |
| `tx_dropped_total` (with `reason` tag) | `send-transaction-service/src/send_transaction_service.rs:385` | reason = "retry_overflow" | Per accounting cycle |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:427` | reason = "without_parsing" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:433` | reason = "parsing_and_sanitization" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:439` | reason = "lock_validation" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:445` | reason = "compute_budget" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:451` | reason = "age" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:454` | reason = "already_processed" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:460` | reason = "fee_payer" | Per scheduler iteration with such drops |
| `tx_dropped_total` (with `reason` tag) | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:463` | reason = "capacity" | Per scheduler iteration with such drops |
| `tx_expired_total` | `send-transaction-service/src/send_transaction_service.rs:514` | Blockhash too old in `process_transactions` | Per expired retry-pool tx |
| `tx_expired_total` | `send-transaction-service/src/send_transaction_service.rs:528` | Same path, alternate branch | Per expired tx |
| `tx_expired_total` (with `tx_type` tag) | `send-transaction-service/src/send_transaction_service.rs:516` | Same, per type | Per (expired tx × type) |
| `tx_expired_total` (with `tx_type` tag) | `send-transaction-service/src/send_transaction_service.rs:530` | Same | Per (expired tx × type) |

### 1.4 Network / consensus counters

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `confirmation_timeout_rate` | `rpc/src/signature_metrics_tracker.rs:191` | Sweep finds entries older than `CONFIRMATION_TIMEOUT` (60 s) | Every 5 s (sweeper-gated; sweeper currently never started) |
| `account_lock_conflict_rate` | `core/src/banking_stage/consumer.rs:280` | AccountInUse retry inside consumer | Per lock-conflict event (high under contention) |
| `retry_due_to_account_in_use_rate` | `core/src/banking_stage/consumer.rs:281` | Same call site, paired counter | Per lock-conflict event |
| `fork_rate` | `core/src/replay_stage.rs:4389` | Fork detected during replay | Per fork-creation event |
| `duplicate_confirmed_blocks` | `core/src/replay_stage.rs:1970` | Duplicate-confirmed block detected | Per duplicate-confirmed event |

### 1.5 Scheduler gauges

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `mempool_size` | `send-transaction-service/src/send_transaction_service.rs:392` | After processing retry batch | Per retry-thread iteration (~`retry_rate_ms`) |
| `mempool_size` | `send-transaction-service/src/send_transaction_service.rs:428` | Retry-thread exit / final emit | Once per cycle |
| `scheduler_buffer_size` | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:202` | Inside scheduler `run()` after work | Once per `should_report` interval (post-fix `ccee6cd252`) |
| `scheduler_buffer_queue_size` | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:205` | Same | Same gating |
| `scheduler_buffer_capacity` | `core/src/banking_stage/transaction_scheduler/scheduler_controller.rs:100` | `SchedulerController` construction | Once at startup |

### 1.6 Lock-accounting

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `avg_locked_accounts_per_tx` | `core/src/banking_stage/transaction_scheduler/receive_and_buffer.rs:314` | Per tx pulled into buffer | **Per buffered tx (ungated; potential flood at high TPS)** |

### 1.7 Blockhash tracking

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `blockhash_fetch_latency_us` | `metrics/src/custom_metrics.rs:207` (definition; never called) | — | Dead code |
| `blockhash_age_at_submit_slots` | `rpc/src/rpc.rs:3762` | `sendTransaction` accepts blockhash, age computed | Per RPC tx submission |
| `blockhash_age_at_submit_slots` | `rpc/src/rpc.rs:3850` | `simulateTransaction` path | Per simulate call |
| `blockhash_remaining_validity_slots` | `rpc/src/rpc.rs:3769` | `sendTransaction` (paired with age) | Per RPC tx submission |
| `blockhash_remaining_validity_slots` | `rpc/src/rpc.rs:3869` | `simulateTransaction` path | Per simulate call |

### 1.8 Lifecycle latencies (driven by `signature_metrics_tracker`)

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `time_to_processed_us` | `rpc/src/signature_metrics_tracker.rs:62` | Inside `observe_processed_once`, first time tx hits processed | Per processed-once flag flip (TRACKER_ACTIVE) |
| `time_to_confirmed_us` | `rpc/src/signature_metrics_tracker.rs:104` | Inside `mark_confirmed` (single-signature path) | Per confirmed signature notification |
| `time_to_confirmed_us` | `rpc/src/signature_metrics_tracker.rs:115` | Inside `mark_confirmed_up_to_slot` (bulk) | Per (slot × confirmed-signature) on each bank notification |
| `time_to_finalized_us` | `rpc/src/signature_metrics_tracker.rs:130` | Inside `mark_finalized` removal | Per finalized signature |

### 1.9 Pipeline latencies (RPC ingress → execution)

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `node_ingress_latency_us` | `metrics/src/custom_metrics.rs:249` (definition; never called) | — | Dead code |
| `node_ingress_latency_us` (with `tx_type`) | `metrics/src/custom_metrics.rs:256` (definition; never called) | — | Dead code |
| `mempool_acceptance_latency_us` | `send-transaction-service/src/send_transaction_service.rs:320` | Tx accepted into mempool | Per accepted RPC tx |
| `mempool_acceptance_latency_us` (with `tx_type`) | `send-transaction-service/src/send_transaction_service.rs:328` | Same, per type | Per (accepted tx × type) |
| `decision_response_latency_us` | `metrics/src/custom_metrics.rs:281` (definition; never called) | — | Dead code |
| `decision_response_latency_us` (with `tx_type`) | `metrics/src/custom_metrics.rs:288` (definition; never called) | — | Dead code |
| `node_to_decision_response_latency_us` | `metrics/src/custom_metrics.rs:297` (definition; never called) | — | Dead code |
| `node_to_decision_response_latency_us` (with `tx_type`) | `metrics/src/custom_metrics.rs:304` (definition; never called) | — | Dead code |

### 1.10 Block-level latencies

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `tx_execution_in_block_latency_us` | `rpc/src/signature_metrics_tracker.rs:94` | Inside `mark_processed_with_slot`, first slot inclusion | Per first-slot inclusion of tracked sig (TRACKER_ACTIVE) |
| `block_production_latency_us` | `rpc/src/signature_metrics_tracker.rs:157` | Inside `mark_block_produced`, per signature in slot | Per (produced slot × tracked sig) (TRACKER_ACTIVE) |
| `state_update_notification_latency_us` | `rpc/src/rpc_subscriptions.rs:906` | Slot-status notification queued | Per state-update event |

### 1.11 Acknowledge latencies

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `acknowledge_latency_us` | `send-transaction-service/src/send_transaction_service.rs:323` | Accepted tx ACK measured | Per accepted RPC tx |
| `acknowledge_latency_us` (with `tx_type`) | `send-transaction-service/src/send_transaction_service.rs:332` | Same, per type | Per (accepted tx × type) |

---

## 2. Signature metrics tracker — `rpc/src/signature_metrics_tracker.rs`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `register_signature` (state) | `rpc/src/signature_metrics_tracker.rs:209` (definition; never called) | Producer not wired | **Never (half-finished)** |
| `mark_processed` (→ `time_to_processed_us`) | `rpc/src/rpc_subscriptions.rs:1135` | `signatureSubscribe` notification — Processed level | Per processed notification (TRACKER_ACTIVE) |
| `mark_processed_with_slot` (→ `time_to_processed_us` + `tx_execution_in_block_latency_us`) | `rpc/src/transaction_status_service.rs:202` | Tx committed, status service emitting | Per committed tx (TRACKER_ACTIVE) |
| `mark_confirmed` (→ `time_to_confirmed_us`) | `rpc/src/rpc_subscriptions.rs:1131` | `signatureSubscribe` notification — Confirmed | Per confirmed notification (TRACKER_ACTIVE) |
| `mark_confirmed_up_to_slot` (→ `time_to_confirmed_us`) | `rpc/src/rpc_subscriptions.rs:842` | `NotificationEntry::Bank` — confirmed slot batch | Per bank notification (~10/s at 100 ms slot, TRACKER_ACTIVE) |
| `mark_confirmed_up_to_slot` (→ `time_to_confirmed_us`) | `rpc/src/rpc_subscriptions.rs:860` | `NotificationEntry::Gossip` — confirmed slot batch | Per gossip notification |
| `mark_finalized` (→ `time_to_finalized_us`) | `rpc/src/rpc_subscriptions.rs:1127` | `signatureSubscribe` notification — Finalized | Per finalized notification (TRACKER_ACTIVE) |
| `mark_finalized_up_to_slot` (→ `time_to_finalized_us`) | `rpc/src/rpc_subscriptions.rs:845` | `NotificationEntry::Bank` — finalized slot batch | Per bank notification (TRACKER_ACTIVE) |
| `mark_block_produced` (→ `block_production_latency_us`) | `rpc/src/transaction_status_service.rs:296` | Status service after block freeze | Per produced block (TRACKER_ACTIVE) |
| `is_tracked` (predicate) | `rpc/src/transaction_status_service.rs:187` | Decide whether to emit per-tx datapoints | Per committed tx (TRACKER_ACTIVE) |
| `start_timeout_sweeper` (background sweeper) | `rpc/src/signature_metrics_tracker.rs:291` (definition; never called) | Sweeper thread not spawned | **Never (half-finished)** |

---

## 3. TX type rules — `rpc/src/tx_type_rules.rs`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `set_rules` (loads global rule map) | `rpc/src/tx_type_rules.rs:24` (definition; never called) | No CLI / config loader wired (`--rpc-tx-type-map-config` missing) | **Never (half-finished)** |
| `infer_types_from_message` (returns labels) | `rpc/src/tx_type_rules.rs:46` (definition; called from `infer_types_from_transaction`) | RPC accept path | Per RPC tx (early-return when rule map empty) |
| `infer_types_from_transaction` (returns labels) | `rpc/src/transaction_status_service.rs:189` | Status service classifying committed tx | Per committed tx (early-return when rule map empty) |
| `parse_discriminator` (utility) | `rpc/src/tx_type_rules.rs:72` (definition; utility) | Used by future config parser | Once per rule load |
| `anchor_discriminator_from_method_name` (utility) | `rpc/src/tx_type_rules.rs:106` (definition; utility) | Same | Once per rule load |

---

## 4. Component-level metrics

### 4.1 Banking stage — `core/src/banking_stage/`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `banking_stage-vote_slot_packet_counts` | `core/src/banking_stage/leader_slot_metrics.rs:193` | Leader slot rotation (`report_slot()`) | Once per leader slot |
| `banking_stage-vote_slot_transaction_errors` | `core/src/banking_stage/leader_slot_metrics.rs:280` | `report_transaction_error_metrics(slot)` | Once per leader slot |
| `banking_stage-vote_packet_counts` | `core/src/banking_stage/leader_slot_metrics.rs:451` | Inside leader-slot report cycle | Once per leader slot |
| `banking_stage_worker_counts` (with `id` tag) | `core/src/banking_stage/consume_worker.rs::ConsumeWorkerCountMetrics::report_and_reset` | `maybe_report_and_reset()` reaches interval threshold | Periodic per worker (`should_report` interval) |
| `banking_stage_worker_timings` (with `id` tag) | `core/src/banking_stage/consume_worker.rs::ConsumeWorkerTimingMetrics::report_and_reset` | Same | Periodic per worker |
| `banking_stage_worker_errors` (with `id` tag) | `core/src/banking_stage/consume_worker.rs::ConsumeWorkerErrorMetrics::report_and_reset` | Same | Periodic per worker |
| `scheduling_details` | `core/src/banking_stage/transaction_scheduler/scheduler_metrics.rs:463` | `SchedulingDetails::maybe_report` | Every 20 ms (`REPORT_INTERVAL`) |
| `banking_stage_count` | `core/src/banking_stage/transaction_scheduler/scheduler_metrics.rs:147` | `SchedulerCountMetrics::maybe_report_and_reset_interval(should_report)` | Periodic (`should_report`-gated) |
| `banking_stage_timing` | `core/src/banking_stage/transaction_scheduler/scheduler_metrics.rs:360` | `SchedulerTimingMetrics::maybe_report_and_reset_interval(should_report)` | Periodic (`should_report`-gated) |
| `banking_stage-leader_slot_packet_counts` etc. | `core/src/banking_stage/leader_slot_timing_metrics.rs::LeaderExecuteAndCommitTimings::report` | Post-execute + post-commit per leader slot | Once per leader slot |

### 4.2 Replay stage — `core/src/replay_stage.rs`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `replay-loop-voting-stats` | `core/src/replay_stage.rs` (multiple sites; periodic emitter) | Replay loop iteration | Periodic |
| `tower-observed` | `core/src/replay_stage.rs` (tower update path) | After tower update | Per voted slot |
| `fork_rate` (M1) | `core/src/replay_stage.rs:4389` | Fork detection | Per fork |
| `duplicate_confirmed_blocks` (M1) | `core/src/replay_stage.rs:1970` | Duplicate confirmation detection | Per duplicate-confirmed event |

### 4.3 Runtime / bank — `runtime/src/bank/metrics.rs`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `bank-new_from_parent-new_epoch_timings` | `runtime/src/bank/metrics.rs::report_new_epoch_metrics` | Epoch transition during `Bank::new_from_parent` | Once per epoch |
| `bank-new_from_parent-new_bank` | `runtime/src/bank/metrics.rs::report_new_bank_metrics` | Bank cloning into descendant | Per bank cloning event |

### 4.4 Gossip — `gossip/src/cluster_info_metrics.rs`

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| `cluster_info_stats` | `gossip/src/cluster_info_metrics.rs` (background reporter) | Periodic background thread | Periodic |
| `cluster_info_vote` | `gossip/src/cluster_info_metrics.rs` (vote path) | Vote received via gossip | Per vote |
| `cluster_info_push` | `gossip/src/cluster_info_metrics.rs` (push path) | Gossip push event | Per push |
| `cluster_info_pull` | `gossip/src/cluster_info_metrics.rs` (pull path) | Gossip pull event | Per pull |
| `cluster_info_prune` | `gossip/src/cluster_info_metrics.rs` (prune path) | Prune message received | Per prune |
| (3 more cluster-info datapoints — see `cluster_info_metrics.rs` source) | `gossip/src/cluster_info_metrics.rs` | Various gossip events | Event-driven |

### 4.5 Other components

| Datapoint | file:line | Trigger | Frequency |
|---|---|---|---|
| Consensus / votor metrics | `votor/src/consensus_metrics.rs` (multiple emit sites) | Vote / consensus state transitions | Event-driven |
| Broadcast stage metrics | `turbine/src/broadcast_stage/broadcast_metrics.rs` (multiple emit sites) | Shred broadcast cycle | Per broadcast batch |
| TPU client send metrics | `tpu-client-next/src/metrics.rs` (multiple emit sites) | TPU client sends | Per send |
| Blockstore RocksDB CF stats | `ledger/src/blockstore_metrics.rs` + `blockstore_metric_report_service.rs` | Periodic background reporter | Periodic |
| SVM transaction-error counters | `svm/src/transaction_error_metrics.rs` (struct, no emit) | Aggregated by caller | N/A (caller emits) |
