use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-env-changed=ZED_RELEASE_CHANNEL");
    println!("cargo::rerun-if-changed=../zed/RELEASE_CHANNEL");
    let channel = match env::var("ZED_RELEASE_CHANNEL") {
        Ok(channel) => channel,
        Err(env::VarError::NotPresent) if env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some() => {
            "dev".into()
        }
        Err(env::VarError::NotPresent) => match fs::read_to_string("../zed/RELEASE_CHANNEL") {
            Ok(channel) => channel,
            // Standalone vendored crates may not include the application crate.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "dev".into(),
            Err(error) => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
    };
    let (name, directory) = match channel.trim() {
        "dev" => ("Koda Dev", "koda-dev"),
        "nightly" => ("Koda Nightly", "koda-nightly"),
        _ => ("Koda", "koda"),
    };
    println!("cargo::rustc-env=KODA_PROFILE_NAME={name}");
    println!("cargo::rustc-env=KODA_PROFILE_DIRECTORY={directory}");
    Ok(())
}
