//! Durable file replacement, for state files and exports.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Replace `path` with `bytes` so that a crash leaves either the old file or
/// the new one, never a mix: write a temporary file beside it, flush it to
/// disk, rename it over `path`, then flush the directory entry. The file is
/// readable by its owner only (mode 0600) on Unix.
///
/// # Errors
///
/// If any step fails; `path` is then unchanged.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp_name = path.file_name().context("no file name")?.to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    #[cfg(unix)]
    {
        // `mode` applies only when the file is created; a leftover
        // temporary file keeps its old permissions otherwise.
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    sync_dir(path);
    Ok(())
}

/// Flush the directory holding `path`, so the rename itself survives a
/// power loss. Not every platform can open a directory for this; failure
/// only weakens durability, so it is logged rather than returned.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
            tracing::debug!(%e, dir = %dir.display(), "directory sync failed");
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!dir.path().join("state.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
