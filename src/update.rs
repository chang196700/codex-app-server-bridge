//! Download and validate an official release before changing any executable.

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    process::Command,
    time::Duration,
};

use crate::service::{self, ServiceOptions};

const REPOSITORY: &str = "chang196700/codex-app-server-bridge";
const PACKAGE: &str = "codex-app-server-bridge";
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
}

pub fn run(service_options: Option<&ServiceOptions>) -> Result<()> {
    let destination = if let Some(options) = service_options {
        service::inspect_installed(options)?.context(
            "service is not installed; run `service install` first, or use `update --self`",
        )?;
        options.executable.clone()
    } else {
        std::env::current_exe().context("locate the current executable")?
    };
    let metadata = std::fs::symlink_metadata(&destination)
        .with_context(|| format!("cannot read executable {}", destination.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "update target must be a regular executable file"
    );
    let current = executable_version(&destination)?;
    let target = release_target()?;
    println!("Checking GitHub Releases for {target} (installed: {current})...");
    // Keep HTTPS independent of native TLS libraries, including static musl
    // builds. Pin the provider explicitly rather than relying on global state.
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let client = Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!(
            "codex-app-server-bridge/",
            env!("CARGO_PKG_VERSION")
        ))
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build()?;
    let release: Release = serde_json::from_slice(&get_bytes(
        &client,
        &format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
        4 * 1024 * 1024,
    )?)
    .context("invalid GitHub release metadata")?;
    ensure!(
        !release.draft && !release.prerelease,
        "GitHub did not return a stable published release"
    );
    let latest = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .context("release tag must start with v")?,
    )
    .context("invalid release version")?;
    ensure!(
        latest.pre.is_empty(),
        "latest release has a prerelease version"
    );
    if !current.cmp_precedence(&latest).is_lt() {
        println!(
            "{} is already up to date ({current}; latest: {latest}).",
            destination.display()
        );
        return Ok(());
    }

    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    let basename = format!("{PACKAGE}-{}-{target}", release.tag_name);
    let archive_name = format!("{basename}.{extension}");
    let archive_asset = asset(&release, &archive_name)?;
    ensure!(
        archive_asset.size > 0 && archive_asset.size <= MAX_ARCHIVE,
        "release archive exceeds the download limit"
    );
    let checksums_asset = asset(&release, "SHA256SUMS")?;
    ensure!(
        checksums_asset.size <= 64 * 1024,
        "release checksums exceed the download limit"
    );
    let checksums = get_bytes(&client, &checksums_asset.browser_download_url, 64 * 1024)?;
    let expected_hash = checksum(std::str::from_utf8(&checksums)?, &archive_name)?;
    let temporary = tempfile::Builder::new()
        .prefix("codex-bridge-update-")
        .tempdir()?;
    let archive_path = temporary.path().join(&archive_name);
    println!("Downloading {} -> {}...", current, latest);
    download_archive(&client, archive_asset, &archive_path, &expected_hash)?;
    let executable_name = if cfg!(windows) {
        "codex-app-server-bridge.exe"
    } else {
        PACKAGE
    };
    let candidate = temporary.path().join(executable_name);
    extract_binary(
        &archive_path,
        &format!("{basename}/{executable_name}"),
        &candidate,
    )?;
    ensure!(
        executable_version(&candidate)? == latest,
        "downloaded executable version does not match the release tag"
    );

    // Another updater may have completed while this download was in progress.
    // Refuse to install an older or unexpected version over its result.
    ensure_version(&destination, &current)?;
    if let Some(options) = service_options {
        service::update_binary(options, &candidate, &current)?;
    } else {
        crate::update_replace::replace_self(&candidate, &current)?;
    }
    println!(
        "Updated {}: {} -> {}",
        destination.display(),
        current,
        latest
    );
    Ok(())
}

fn release_target() -> Result<String> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("GitHub Releases do not support architecture {other}"),
    };
    let platform = if cfg!(target_os = "windows") && cfg!(target_env = "msvc") {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") && cfg!(target_env = "musl") {
        "unknown-linux-musl"
    } else if cfg!(target_os = "linux") && cfg!(target_env = "gnu") {
        "unknown-linux-gnu"
    } else {
        bail!("GitHub Releases do not support this build target");
    };
    Ok(format!("{architecture}-{platform}"))
}

fn asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .with_context(|| format!("release {} is missing {name}", release.tag_name))?;
    ensure!(matches.next().is_none(), "duplicate release asset {name}");
    let expected_url = format!(
        "https://github.com/{REPOSITORY}/releases/download/{}/{name}",
        release.tag_name
    );
    ensure!(
        asset.browser_download_url == expected_url,
        "release asset URL does not belong to the expected GitHub release"
    );
    Ok(asset)
}

fn get_bytes(client: &Client, url: &str, limit: u64) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .with_context(|| format!("request {url}"))?
        .error_for_status()
        .context("GitHub request failed (check connectivity and the public API rate limit)")?;
    ensure!(
        response.content_length().is_none_or(|size| size <= limit),
        "GitHub response exceeds the size limit"
    );
    let mut bytes = Vec::new();
    response.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "GitHub response exceeds the size limit"
    );
    Ok(bytes)
}

fn checksum(document: &str, name: &str) -> Result<String> {
    let mut result = None;
    for line in document.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(hash), Some(filename), None) = (fields.next(), fields.next(), fields.next())
            && filename.trim_start_matches('*') == name
        {
            ensure!(result.is_none(), "duplicate archive checksum");
            ensure!(
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid archive checksum"
            );
            result = Some(hash.to_ascii_lowercase());
        }
    }
    result.with_context(|| format!("SHA256SUMS does not contain {name}"))
}

fn download_archive(
    client: &Client,
    asset: &Asset,
    path: &Path,
    expected_hash: &str,
) -> Result<()> {
    let response = client
        .get(&asset.browser_download_url)
        .send()?
        .error_for_status()?;
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size == asset.size),
        "release archive size changed"
    );
    let mut reader = response.take(asset.size + 1);
    let mut output = File::create_new(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= asset.size,
            "release archive exceeds its declared size"
        );
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    ensure!(
        total == asset.size,
        "release archive download is incomplete"
    );
    let mut actual_hash = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        write!(&mut actual_hash, "{byte:02x}")?;
    }
    ensure!(
        actual_hash == expected_hash,
        "release archive SHA-256 mismatch; nothing was installed"
    );
    output.sync_all()?;
    Ok(())
}

fn save_binary(reader: &mut impl Read, size: u64, destination: &Path) -> Result<()> {
    ensure!(
        size > 0 && size <= MAX_BINARY,
        "release executable exceeds the size limit"
    );
    let mut output = File::create_new(destination)?;
    let count = std::io::copy(&mut reader.take(size + 1), &mut output)?;
    ensure!(
        count == size,
        "release executable size does not match the archive"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    output.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn extract_binary(archive: &Path, entry_name: &str, destination: &Path) -> Result<()> {
    let compressed = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut archive = tar::Archive::new(compressed.take(MAX_ARCHIVE * 4));
    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new(entry_name) {
            ensure!(
                !found && entry.header().entry_type().is_file(),
                "archive must contain exactly one regular executable"
            );
            let size = entry.size();
            save_binary(&mut entry, size, destination)?;
            found = true;
        }
    }
    ensure!(found, "release archive is missing {entry_name}");
    Ok(())
}

#[cfg(windows)]
fn extract_binary(archive: &Path, entry_name: &str, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
    let mut found = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        // PowerShell Compress-Archive uses backslashes in some releases.
        if entry.name()?.replace('\\', "/") == entry_name {
            ensure!(
                !found && entry.is_file() && !entry.is_symlink(),
                "archive must contain exactly one regular executable"
            );
            let size = entry.size();
            save_binary(&mut entry, size, destination)?;
            found = true;
        }
    }
    ensure!(found, "release archive is missing {entry_name}");
    Ok(())
}

pub(crate) fn ensure_version(path: &Path, expected: &Version) -> Result<()> {
    ensure!(
        executable_version(path)? == *expected,
        "target executable changed during download; rerun update"
    );
    Ok(())
}

fn executable_version(path: &Path) -> Result<Version> {
    let mut command = Command::new(path);
    command.arg("--version").stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .with_context(|| format!("read executable version from {}", path.display()))?;
    ensure!(
        output.status.success(),
        "{} --version failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let text = std::str::from_utf8(&output.stdout)?.trim();
    Version::parse(
        text.strip_prefix("codex-app-server-bridge ")
            .context("unexpected executable version output")?,
    )
    .context("invalid executable version")
}
