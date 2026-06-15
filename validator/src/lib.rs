#![allow(clippy::arithmetic_side_effects)]
pub use solana_test_validator as test_validator;
use {
    console::style,
    fd_lock::{RwLock, RwLockWriteGuard},
    indicatif::{ProgressDrawTarget, ProgressStyle},
    std::{
        borrow::Cow,
        fmt::Display,
        fs::{File, OpenOptions},
        path::Path,
        process::exit,
        time::Duration,
    },
};

pub mod admin_rpc_service;
pub mod bootstrap;
pub mod cli;
pub mod commands;
pub mod dashboard;
pub mod hot_accounts;

pub fn format_name_value(name: &str, value: &str) -> String {
    format!("{} {}", style(name).bold(), value)
}

/// Whether single-validator mode is enabled, honoring both the
/// `--single-validator` CLI flag and the `SINGLE_VALIDATOR` environment
/// variable. clap 2.x ignores `.env()` on no-value flags (its `add_env`
/// resolves only opts/positionals), so the env var is resolved here. A set
/// env var counts as enabled unless its trimmed value is empty, `0`, `false`,
/// `no`, or `off` (case-insensitive).
pub fn single_validator_enabled(matches: &clap::ArgMatches) -> bool {
    if matches.is_present("single_validator") {
        return true;
    }
    match std::env::var("SINGLE_VALIDATOR") {
        Ok(value) => {
            let value = value.trim();
            !value.is_empty()
                && !value.eq_ignore_ascii_case("0")
                && !value.eq_ignore_ascii_case("false")
                && !value.eq_ignore_ascii_case("no")
                && !value.eq_ignore_ascii_case("off")
        }
        Err(_) => false,
    }
}
/// Pretty print a "name value"
pub fn println_name_value(name: &str, value: &str) {
    println!("{}", format_name_value(name, value));
}

/// Creates a new process bar for processing that will take an unknown amount of time
pub fn new_spinner_progress_bar() -> ProgressBar {
    let progress_bar = indicatif::ProgressBar::new(42);
    progress_bar.set_draw_target(ProgressDrawTarget::stdout());
    progress_bar.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {wide_msg}")
            .expect("ProgresStyle::template direct input to be correct"),
    );
    progress_bar.enable_steady_tick(Duration::from_millis(100));

    ProgressBar {
        progress_bar,
        is_term: console::Term::stdout().is_term(),
    }
}

pub struct ProgressBar {
    progress_bar: indicatif::ProgressBar,
    is_term: bool,
}

impl ProgressBar {
    pub fn set_message<T: Into<Cow<'static, str>> + Display>(&self, msg: T) {
        if self.is_term {
            self.progress_bar.set_message(msg);
        } else {
            println!("{msg}");
        }
    }

    pub fn println<I: AsRef<str>>(&self, msg: I) {
        self.progress_bar.println(msg);
    }

    pub fn abandon_with_message<T: Into<Cow<'static, str>> + Display>(&self, msg: T) {
        if self.is_term {
            self.progress_bar.abandon_with_message(msg);
        } else {
            println!("{msg}");
        }
    }
}

pub fn ledger_lockfile(ledger_path: &Path) -> RwLock<File> {
    let lockfile = ledger_path.join("ledger.lock");
    fd_lock::RwLock::new(
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lockfile)
            .unwrap(),
    )
}

pub fn lock_ledger<'lock>(
    ledger_path: &Path,
    ledger_lockfile: &'lock mut RwLock<File>,
) -> RwLockWriteGuard<'lock, File> {
    ledger_lockfile.try_write().unwrap_or_else(|_| {
        println!(
            "Error: Unable to lock {} directory. Check if another validator is running",
            ledger_path.display()
        );
        exit(1);
    })
}
