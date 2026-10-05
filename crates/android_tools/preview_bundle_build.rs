use anyhow::{Context as _, Result, bail, ensure};
use fs2::FileExt as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

#[derive(Deserialize)]
struct Artifact {
    url: String,
    sha256: String,
}

#[derive(Deserialize)]
struct InstalledArtifact {
    path: String,
    #[serde(flatten)]
    artifact: Artifact,
}

#[derive(Deserialize)]
struct Manifest {
    protocol: String,
    artifacts: Vec<InstalledArtifact>,
    compiler: Artifact,
    java: BTreeMap<String, Artifact>,
    java_source: String,
    layoutlib: BTreeMap<String, Artifact>,
}

pub fn build() -> Result<()> {
    for file in [
        "preview_bundle_build.rs",
        "preview-bundle.json",
        "src/PreviewBridge.java",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    for variable in ["KODA_COMPOSE_PREVIEW_ARTIFACT_CACHE", "CARGO_NET_OFFLINE"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let manifest: Manifest = serde_json::from_str(include_str!("preview-bundle.json"))?;
    let platform = format!(
        "{}-{}",
        env::var("CARGO_CFG_TARGET_OS")?,
        env::var("CARGO_CFG_TARGET_ARCH")?
    );
    let Some(native) = manifest.layoutlib.get(&platform) else {
        println!(
            "cargo:warning=Google layoutlib does not provide a Compose preview runtime for {platform}"
        );
        return Ok(());
    };
    let host = env::var("HOST")?;
    let host_os = if host.contains("apple-darwin") {
        "macos"
    } else if host.contains("windows") {
        "windows"
    } else if host.contains("linux") {
        "linux"
    } else {
        bail!("Cannot compile the Compose bridge on host {host}")
    };
    let host_arch = host
        .split('-')
        .next()
        .context("Missing build host architecture")?;
    let host_java = manifest
        .java
        .get(&format!("{host_os}-{host_arch}"))
        .context("No pinned Java runtime for the build host")?;
    let target_java = manifest
        .java
        .get(&platform)
        .context("No pinned preview Java runtime")?;
    let output = PathBuf::from(env::var_os("OUT_DIR").context("Missing OUT_DIR")?);
    let cache = env::var_os("KODA_COMPOSE_PREVIEW_ARTIFACT_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("CARGO_HOME").map(|home| PathBuf::from(home).join("koda-preview-artifacts"))
        })
        .unwrap_or_else(|| output.join("downloads"));
    fs::create_dir_all(&cache)?;
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" || attempt.previous().len() >= 10 {
                attempt.error("Unsafe Compose preview download redirect")
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .user_agent("Koda-Compose-Preview-Build")
        .build()?;
    let staging = tempfile::tempdir_in(&output)?;
    let distribution = staging.path().join("distribution");
    fs::create_dir(&distribution)?;
    let mut sources = Vec::new();
    for artifact in &manifest.artifacts {
        let path = distribution.join(&artifact.path);
        fs::create_dir_all(path.parent().context("Artifact has no parent directory")?)?;
        fs::copy(download(&client, &cache, &artifact.artifact)?, path)?;
        sources.push(artifact.artifact.url.clone());
    }
    ZipArchive::new(fs::File::open(download(&client, &cache, native)?)?)?
        .extract(distribution.join("layoutlib"))?;
    sources.push(native.url.clone());
    unpack_java(
        &download(&client, &cache, target_java)?,
        &target_java.url,
        &distribution.join("java"),
        staging.path(),
    )?;
    sources.push(target_java.url.clone());
    let compiler_java = if target_java.sha256 == host_java.sha256 {
        distribution.join("java")
    } else {
        let directory = staging.path().join("host-java");
        unpack_java(
            &download(&client, &cache, host_java)?,
            &host_java.url,
            &directory,
            staging.path(),
        )?;
        directory
    };
    let compiler = download(&client, &cache, &manifest.compiler)?;
    let java_name = if host_os == "windows" {
        "java.exe"
    } else {
        "java"
    };
    let compiled = Command::new(compiler_java.join("bin").join(java_name))
        .args(["-jar"])
        .arg(compiler)
        .args(["-21", "-proc:none", "-warn:none", "-classpath"])
        .arg(distribution.join("renderer.jar"))
        .arg("-d")
        .arg(&distribution)
        .arg("src/PreviewBridge.java")
        .output()
        .context("Could not compile the bundled Compose preview bridge")?;
    ensure!(
        compiled.status.success(),
        "Compose bridge compilation failed:\n{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::write(
        distribution.join(".protocol"),
        format!("{}\n", manifest.protocol),
    )?;
    fs::write(
        distribution.join("SOURCE.txt"),
        format!(
            "Google Android tooling: Apache-2.0; preserve JAR and layoutlib notices.\nEclipse Temurin: GPL-2.0 with Classpath Exception; licenses are in java/legal.\nCorresponding Java source: {}\nBuild compiler (not shipped): {}\n{}\n",
            manifest.java_source,
            manifest.compiler.url,
            sources.join("\n")
        ),
    )?;
    let archive_path = output.join("compose-preview.zip");
    let archive = tempfile::NamedTempFile::new_in(&output)?;
    bundle(&distribution, archive.as_file())?;
    let id = hash_file(archive.path())?;
    archive
        .persist(&archive_path)
        .map_err(|error| error.error)?;
    println!("cargo:rustc-cfg=compose_preview_bundled");
    println!("cargo:rustc-env=KODA_COMPOSE_PREVIEW_BUNDLE_ID={id}");
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn download(
    client: &reqwest::blocking::Client,
    cache: &Path,
    artifact: &Artifact,
) -> Result<PathBuf> {
    ensure!(
        artifact.url.starts_with("https://")
            && artifact.sha256.len() == 64
            && artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid pinned preview artifact"
    );
    let path = cache.join(&artifact.sha256);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 2 * 1024 * 1024 * 1024,
            "Unsafe preview artifact cache entry"
        );
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join(format!("{}.lock", artifact.sha256)))?;
    lock.lock_exclusive()?;
    if path.is_file() && hash_file(&path)? == artifact.sha256 {
        return Ok(path);
    }
    ensure!(
        !env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
        "Missing cached Compose preview artifact in offline build: {}",
        artifact.url
    );
    eprintln!(
        "Fetching bundled Compose preview dependency: {}",
        artifact.url
    );
    let mut last_error = None;
    for _ in 0..3 {
        let result = (|| {
            let response = client.get(&artifact.url).send()?.error_for_status()?;
            ensure!(
                response.url().scheme() == "https",
                "Unsafe preview artifact URL"
            );
            let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
            let maximum = 2 * 1024 * 1024 * 1024;
            ensure!(
                response.content_length().is_none_or(|size| size <= maximum),
                "Preview artifact exceeds 2 GiB"
            );
            ensure!(
                io::copy(&mut response.take(maximum + 1), &mut temporary)? <= maximum,
                "Preview artifact exceeds 2 GiB"
            );
            temporary.flush()?;
            ensure!(
                hash_file(temporary.path())? == artifact.sha256,
                "Checksum mismatch for {}",
                artifact.url
            );
            temporary.persist(&path).map_err(|error| error.error)?;
            Ok::<_, anyhow::Error>(())
        })();
        match result {
            Ok(()) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.context("Compose artifact download did not run")?).with_context(|| {
        format!(
            "Could not fetch bundled preview dependency {}",
            artifact.url
        )
    })
}

fn unpack_java(archive: &Path, url: &str, destination: &Path, staging: &Path) -> Result<()> {
    let extracted = tempfile::tempdir_in(staging)?;
    if url.ends_with(".zip") {
        ZipArchive::new(fs::File::open(archive)?)?.extract(extracted.path())?;
    } else {
        let decoded = flate2::read::GzDecoder::new(fs::File::open(archive)?);
        unpack_tar(decoded, extracted.path())?;
    }
    let roots = fs::read_dir(extracted.path())?.collect::<io::Result<Vec<_>>>()?;
    ensure!(
        roots.len() == 1,
        "Java archive must contain one distribution root"
    );
    let root = roots.first().context("Java archive is empty")?.path();
    let home = if root.join("Contents/Home").is_dir() {
        root.join("Contents/Home")
    } else {
        root
    };
    copy_tree(&home, destination)?;
    Ok(())
}

fn unpack_tar(reader: impl io::Read, root: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut links = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let path = entry.path()?.into_owned();
            ensure!(
                path.components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
                "Invalid Java archive link path"
            );
            let target = entry
                .link_name()?
                .context("Java archive link has no target")?
                .into_owned();
            let source = if kind.is_hard_link() {
                root.join(target)
            } else {
                root.join(path.parent().context("Java link has no parent")?)
                    .join(target)
            };
            links.push((root.join(path), source));
        } else {
            ensure!(
                entry.unpack_in(root)?,
                "Java archive contains a path outside its distribution"
            );
        }
    }
    // Windows cannot create Unix symlinks without extra privileges. Resolve archive links
    // into ordinary files after their targets have been unpacked, on every build host.
    let root = root.canonicalize()?;
    while !links.is_empty() {
        let previous = links.len();
        let mut unresolved = Vec::new();
        for (destination, source) in links {
            if !source.try_exists()? {
                unresolved.push((destination, source));
                continue;
            }
            let source = source.canonicalize()?;
            ensure!(
                source.starts_with(&root),
                "Java archive link escapes its distribution"
            );
            fs::create_dir_all(
                destination
                    .parent()
                    .context("Java archive link has no parent")?,
            )?;
            if source.is_dir() {
                copy_tree(&source, &destination)?;
            } else {
                fs::copy(source, destination)?;
            }
        }
        ensure!(
            unresolved.len() < previous,
            "Java archive has unresolved or cyclic links"
        );
        links = unresolved;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        // Dereference links while assembling the bundle so extraction is portable to Windows.
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn bundle(root: &Path, output: &fs::File) -> Result<()> {
    fn collect(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                collect(&path, files)?;
            } else {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(root, &mut files)?;
    files.sort();
    let mut archive = ZipWriter::new(output);
    let mut sizes = BTreeMap::new();
    for path in files {
        let name = path
            .strip_prefix(root)?
            .to_str()
            .context("Non-UTF-8 preview runtime path")?
            .replace('\\', "/");
        let mut options = FileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(1));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            options = options.unix_permissions(fs::metadata(&path)?.permissions().mode());
        }
        // Tar metadata carries executable bits even when the build host is Windows.
        if name.starts_with("java/bin/") || name == "java/lib/jspawnhelper" {
            options = options.unix_permissions(0o755);
        }
        let mode = if name.starts_with("java/bin/") || name == "java/lib/jspawnhelper" {
            0o755
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::metadata(&path)?.permissions().mode() & 0o777
            }
            #[cfg(not(unix))]
            {
                0o644
            }
        };
        options = options.unix_permissions(mode);
        sizes.insert(name.clone(), serde_json::json!({"size": fs::metadata(&path)?.len(), "sha256": hash_file(&path)?, "mode": mode}));
        archive.start_file(name, options)?;
        io::copy(&mut fs::File::open(path)?, &mut archive)?;
    }
    archive.start_file(
        ".files.json",
        FileOptions::default().compression_method(CompressionMethod::Deflated),
    )?;
    archive.write_all(&serde_json::to_vec(&sizes)?)?;
    archive.finish()?;
    Ok(())
}
