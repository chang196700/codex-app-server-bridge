use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Resolve an explicit executable or find Codex without relying on a shell.
pub fn resolve(configured: Option<&Path>, user_home: &Path) -> Result<PathBuf> {
    if let Some(path) = configured {
        return executable(path)
            .with_context(|| format!("invalid configured codex_path: {}", path.display()));
    }

    let path_directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| {
            std::env::split_paths(&value)
                // Empty and relative entries would search the service's working
                // directory, which is unrelated to the intended installation.
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let mut candidates: Vec<PathBuf> = path_directories
        .iter()
        .map(|directory| directory.join(executable_name()))
        .collect();

    for directory in [".local/bin", ".cargo/bin"] {
        candidates.push(user_home.join(directory).join(executable_name()));
    }

    #[cfg(windows)]
    {
        // Derive user installations from the service's configured target home,
        // not LOCALAPPDATA/APPDATA, which refer to LocalSystem in a system service.
        candidates.push(user_home.join("AppData/Local/Programs/OpenAI/Codex/bin/codex.exe"));
        let npm_directory = user_home.join("AppData/Roaming/npm");
        candidates.push(npm_directory.join(executable_name()));
        for directory in path_directories
            .iter()
            .chain(std::iter::once(&npm_directory))
        {
            npm_candidates(directory, &mut candidates);
        }
    }

    #[cfg(target_os = "macos")]
    {
        candidates.push(user_home.join("Applications/Codex.app/Contents/Resources/codex"));
        candidates.push(PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        ));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
    }

    #[cfg(unix)]
    {
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/usr/bin/codex"));
    }

    for candidate in candidates {
        if let Ok(path) = executable(&candidate) {
            return Ok(path);
        }
    }
    bail!("could not find a Codex executable; install Codex or set codex_path in the configuration")
}

fn executable_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}

fn executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("Codex executable path must be absolute");
    }
    #[cfg(windows)]
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        bail!("Codex executable must be an .exe file, not a shell script");
    }
    let resolved = path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let metadata = std::fs::metadata(&resolved)
        .with_context(|| format!("cannot inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("Codex executable is not a regular file: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("Codex file is not executable: {}", path.display());
        }
    }
    // Keep stable shims and symlinks in the service snapshot so a Codex upgrade
    // can replace their targets without leaving a version-specific path behind.
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn npm_candidates(directory: &Path, candidates: &mut Vec<PathBuf>) {
    let (package, target) = match std::env::consts::ARCH {
        "x86_64" => ("codex-win32-x64", "x86_64-pc-windows-msvc"),
        "aarch64" => ("codex-win32-arm64", "aarch64-pc-windows-msvc"),
        _ => return,
    };
    let packages = directory.join("node_modules/@openai");
    // npm may hoist the platform package or keep it beneath @openai/codex.
    // Older Codex releases shipped vendor directly in the main package.
    for package_root in [
        packages.join(package),
        packages.join("codex/node_modules/@openai").join(package),
        packages.join("codex"),
    ] {
        for binary_directory in ["bin", "codex"] {
            candidates.push(
                package_root
                    .join("vendor")
                    .join(target)
                    .join(binary_directory)
                    .join("codex.exe"),
            );
        }
    }
}
