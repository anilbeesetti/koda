//! Rust DAP adapter for Android ART and JVM targets over JDWP.
//!
//! All inspection commands are read-only. This crate does not invoke target
//! methods, evaluate arbitrary code, launch applications, or own ADB forwards.
//! The Android panel owns deployment and forwarding; this adapter owns one TCP
//! connection and its event requests. Kotlin source positions use JSR-45 SMAP.

pub mod dap;
pub mod expression;
pub mod fixture;
pub mod handles;
pub mod runner_fixture;
mod runner_process;
pub mod session;
pub mod source;
pub mod wire;

pub const INSTALL_REVISION: &str = "koda-rust-jdwp-1";

pub fn run() -> anyhow::Result<()> {
    dap::serve(std::io::stdin(), std::io::stdout())
}
