#![allow(clippy::disallowed_methods, reason = "build scripts run synchronously")]

#[cfg(feature = "bundled-preview")]
#[path = "preview_bundle_build.rs"]
mod preview_bundle_build;

fn main() -> anyhow::Result<()> {
    println!("cargo:rustc-check-cfg=cfg(compose_preview_bundled)");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "bundled-preview")]
    preview_bundle_build::build()?;
    Ok(())
}
