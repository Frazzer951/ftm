//! Reading files for the artifact viewer. Paths come from pane output (OSC 8 `file:` links, paths printed by a
//! program), which is untrusted: only absolute paths to regular files are read, after resolving symlinks, and never
//! more than `MAX_BYTES`.

use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const MAX_BYTES: u64 = 512 << 20;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Stat {
    /// With symlinks and `..` resolved.
    pub path: String,
    pub size: u64,
    /// Milliseconds since the epoch; 0 where the platform has none.
    pub modified: u64,
}

pub fn stat(path: &str) -> Result<Stat, String> {
    let (path, meta) = regular(path)?;
    Ok(Stat {
        path: path.to_string_lossy().into_owned(),
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    })
}

pub fn read(path: &str) -> Result<Vec<u8>, String> {
    let (canonical, meta) = regular(path)?;
    if meta.len() > MAX_BYTES {
        return Err(too_big(path));
    }
    let file = File::open(&canonical).map_err(|e| format!("{path}: {e}"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    // The file may have grown since the check.
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_big(path));
    }
    Ok(bytes)
}

fn too_big(path: &str) -> String {
    format!("{path} is larger than {} MiB", MAX_BYTES >> 20)
}

fn regular(path: &str) -> Result<(PathBuf, Metadata), String> {
    if !Path::new(path).is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    let canonical = Path::new(path).canonicalize().map_err(|e| format!("{path}: {e}"))?;
    let meta = std::fs::metadata(&canonical).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    Ok((canonical, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ftm-artifacts-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn reads_regular_files_through_symlinks() {
        let dir = scratch();
        let file = dir.join("plan.md");
        std::fs::write(&file, "# plan").unwrap();
        let link = dir.join("link.md");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read(link.to_str().unwrap()).unwrap(), b"# plan");
        std::fs::create_dir(dir.join("sub")).unwrap();
        let s = stat(dir.join("sub/../link.md").to_str().unwrap()).unwrap();
        assert_eq!(s.path, file.to_str().unwrap());
        assert_eq!(s.size, 6);
        assert!(s.modified > 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_relative_paths_folders_and_devices() {
        let dir = scratch();
        assert!(read("plan.md").unwrap_err().contains("not an absolute path"));
        assert!(read(dir.to_str().unwrap()).unwrap_err().contains("not a file"));
        assert!(read("/dev/zero").unwrap_err().contains("not a file"));
        assert!(stat(dir.join("missing").to_str().unwrap()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
