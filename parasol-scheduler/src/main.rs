use std::time::Duration;

use agave_scheduling_utils::{handshake::ClientLogon, transaction_ptr::{TransactionPtr, TransactionPtrBatch}};
use agave_transaction_view::{transaction_data::TransactionData, transaction_view::SanitizedTransactionView};
use clap::Parser;
use serde::Deserialize;
use solana_runtime_transaction::runtime_transaction::RuntimeTransaction;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: String,
    #[arg(long)]
    config_path: String,
}

#[derive(Deserialize, Debug)]
struct TpuConfig {
    pub worker_count: usize,
    pub allocator_size: usize,
    pub allocator_handles: usize,
    pub tpu_to_pack_size: usize,
    pub progress_tracker_size: usize,
    pub pack_to_worker_size: usize,
    pub worker_to_pack_size: usize,
}


fn default_connect_timeout() -> std::time::Duration { std::time::Duration::from_millis(100) }

#[derive(Deserialize, Debug)]
struct Config {
    pub tpu: TpuConfig,
    #[serde(with = "humantime_serde")]
    #[serde(default="default_connect_timeout")]
    pub connect_timeout: Duration,
    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub spin_delay: Option<Duration>,
}


fn main() {
    let args = Args::parse();
    env_logger::init();

    let config: Config = toml::from_slice(&std::fs::read(args.config_path).unwrap()).unwrap();
    let logon = ClientLogon {
        allocator_size: config.tpu.allocator_size,
        pack_to_worker_size: config.tpu.pack_to_worker_size,
        progress_tracker_size: config.tpu.progress_tracker_size,
        allocator_handles: config.tpu.allocator_handles,
        tpu_to_pack_size: config.tpu.tpu_to_pack_size,
        worker_count: config.tpu.worker_count,
        worker_to_pack_size: config.tpu.worker_to_pack_size
    };

    let session = agave_scheduling_utils::handshake::client::connect(args.socket, logon, config.connect_timeout).unwrap();
    let agave_scheduling_utils::handshake::client::ClientSession{mut tpu_to_pack, allocators, .. } = session;

    loop {
        match tpu_to_pack.try_read() {
            Some(to_pack) => {
                let to_pack = unsafe {to_pack.as_ref()};
                let txptr = unsafe { TransactionPtr::from_sharable_transaction_region(&to_pack.transaction, &allocators[0]) };
                let data: &[u8] = <TransactionPtr as TransactionData>::data(&txptr);
                let Ok(view) = SanitizedTransactionView::try_new_sanitized(data, true) else {
                    log::error!("skip invalid tx");
                    continue;
                };
                let rxtrx = RuntimeTransaction::<SanitizedTransactionView<_>>::try_from(
                    view,
                    solana_transaction::sanitized::MessageHash::Compute,
                    None
                );

            },
            None => {
                if let Some(ref time) = config.spin_delay {
                    std::thread::sleep(*time);
                }
            }
        }
    }
}
