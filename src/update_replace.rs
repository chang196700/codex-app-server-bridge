//! Replace this command's executable without modifying service registration.

use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

/// Install a previously verified candidate over the executable running this command.
/// The caller must finish verifying the candidate before invoking this function.
pub fn replace_self(candidate: &Path, expected_version: &semver::Version) -> Result<()> {
    let reported_path = std::env::current_exe().context("locate the current executable")?;
    regular_file(&reported_path, "current executable")?;
    let destination = reported_path
        .canonicalize()
        .context("resolve the current executable path")?;
    let directory = destination
        .parent()
        .context("current executable has no parent directory")?;
    let directory_metadata =
        fs::symlink_metadata(directory).context("inspect the executable directory")?;
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        bail!("executable directory must be a directory, not a symbolic link");
    }

    // Share the service installer's lock when this command runs from the fixed
    // service executable. The file remains in place so concurrent commands lock
    // the same inode; removing it on unlock would introduce a race.
    let _lock = lock_directory(directory)?;
    let original_metadata = regular_file(&destination, "current executable")?;
    crate::update::ensure_version(&destination, expected_version)?;
    regular_file(candidate, "update candidate")?;
    let mut source = File::open(candidate).context("open the verified update candidate")?;
    let source_metadata = source.metadata()?;
    if !source_metadata.is_file() || source_metadata.len() == 0 {
        bail!("update candidate must be a nonempty regular file");
    }

    let mut staged = tempfile::Builder::new()
        .prefix(".codex-app-server-bridge-update-")
        .suffix(".new")
        .tempfile_in(directory)
        .context("create an update file beside the current executable")?;
    let copied = std::io::copy(&mut source, staged.as_file_mut())
        .context("stage the verified update candidate")?;
    if copied != source_metadata.len() {
        bail!("update candidate changed while being copied");
    }
    drop(source);

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // chown may clear mode bits, so restore the original mode afterwards.
        std::os::unix::fs::chown(
            staged.path(),
            Some(original_metadata.uid()),
            Some(original_metadata.gid()),
        )
        .context("preserve the current executable's owner and group")?;
    }
    staged
        .as_file()
        .set_permissions(original_metadata.permissions())
        .context("preserve the current executable's permissions")?;
    staged
        .as_file()
        .sync_all()
        .context("flush the staged executable")?;

    replace(staged, &destination, directory)
}

fn regular_file(path: &Path, description: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description}: {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "{description} must be a regular file, not a symbolic link: {}",
            path.display()
        );
    }
    Ok(metadata)
}

fn lock_directory(directory: &Path) -> Result<File> {
    let path = directory.join("install.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!(
                "executable update lock must be a regular file: {}",
                path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect the executable update lock"),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(&path)
        .context("open the executable update lock")?;
    if !lock.metadata()?.is_file() {
        bail!("executable update lock is not a regular file");
    }
    regular_file(&path, "executable update lock")?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("another executable update or service installation is in progress")?;
    Ok(lock)
}

#[cfg(unix)]
fn replace(staged: tempfile::NamedTempFile, destination: &Path, directory: &Path) -> Result<()> {
    // A failed rename leaves the original executable in place. A successful
    // rename is atomic, including when another process is running the old inode.
    let installed = staged.persist(destination).map_err(|error| {
        anyhow::anyhow!("cannot replace the current executable; original file retained: {error}")
    })?;
    drop(installed);
    if let Err(error) = File::open(directory).and_then(|directory| directory.sync_all()) {
        tracing::warn!(%error, "executable updated, but its directory could not be flushed");
    }
    Ok(())
}

#[cfg(windows)]
fn replace(staged: tempfile::NamedTempFile, destination: &Path, directory: &Path) -> Result<()> {
    // Windows permits renaming a running image, but not overwriting or deleting
    // it. Reserve a unique sibling name, close its handle, then move the image.
    let (placeholder, parked) = tempfile::Builder::new()
        .prefix(".codex-app-server-bridge-update-")
        .suffix(".previous.exe")
        .tempfile_in(directory)
        .context("reserve a recovery path for the running executable")?
        .keep()
        .context("retain the executable recovery path")?;
    drop(placeholder);
    if let Err(error) = fs::rename(destination, &parked) {
        let _ = fs::remove_file(&parked);
        return Err(error).context("move the running executable aside; original file retained");
    }

    match staged.persist(destination) {
        Ok(installed) => drop(installed),
        Err(error) => {
            return match fs::rename(&parked, destination) {
                Ok(()) => Err(error).context("self-update failed; original executable restored"),
                Err(recovery) => Err(error).context(format!(
                    "self-update failed and the original executable could not be restored: \
                     {recovery}; recovery file retained at {}",
                    parked.display()
                )),
            };
        }
    }

    // This API is specifically for the running image, including after a rename.
    // Its helper waits for this process to exit; the new path is usable now.
    // Cleanup is best effort: the replacement already succeeded, so reporting a
    // failed update here would misleadingly suggest that the old version remains.
    if let Err(error) = self_replace::self_delete_at(&parked) {
        tracing::warn!(path = %parked.display(), %error,
            "executable updated, but cleanup of the previous image could not be scheduled");
    }
    Ok(())
}
