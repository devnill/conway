//! Shared helpers for the `config_*` integration tests: fixture path
//! resolution and unique scratch directories (no external tempfile crate
//! dependency — each test gets its own subdirectory under
//! `std::env::temp_dir()`, disambiguated by an atomic counter so parallel
//! test threads never collide).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config")
}

pub fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-config-test-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}
