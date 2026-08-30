#![forbid(unsafe_code)]

//! Option 3(C) fallback: durability engineering on top of the one snapshot
//! mechanism turbovec's *published* 0.9.0 API gives us for free
//! (`IdMapIndex::write()`/`load()`). Rather than needing `from_parts()` at
//! all, write N checksummed generations and fall through to an older one if
//! the newest is corrupt — an escape hatch that costs nothing beyond a
//! CRC32 sidecar file per snapshot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use turbovec::IdMapIndex;

/// Write an index snapshot plus a CRC32 checksum sidecar, so a later load
/// can detect corruption instead of silently trusting garbage bytes or
/// hitting whatever `IdMapIndex::load()` does with a truncated/corrupt file.
pub fn write_checksummed(index: &IdMapIndex, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    index.write(path)?;
    let bytes = fs::read(path)?;
    let checksum = crc32fast::hash(&bytes);
    fs::write(checksum_sidecar(path), checksum.to_string())
}

fn checksum_sidecar(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".crc32");
    PathBuf::from(s)
}

#[derive(Debug)]
pub enum LoadError {
    /// The snapshot file or its checksum sidecar is missing.
    Missing,
    /// The file's actual CRC32 doesn't match the recorded checksum.
    ChecksumMismatch,
    Io(io::Error),
}

/// Verify the checksum sidecar before trusting the snapshot file at all.
pub fn load_verified(path: impl AsRef<Path>) -> Result<IdMapIndex, LoadError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => LoadError::Missing,
        _ => LoadError::Io(e),
    })?;
    let sidecar = fs::read_to_string(checksum_sidecar(path)).map_err(|_| LoadError::Missing)?;
    let expected: u32 = sidecar
        .trim()
        .parse()
        .map_err(|_| LoadError::ChecksumMismatch)?;
    if crc32fast::hash(&bytes) != expected {
        return Err(LoadError::ChecksumMismatch);
    }
    IdMapIndex::load(path).map_err(LoadError::Io)
}

/// Try each candidate path in order, returning the first that loads and
/// verifies successfully. This is the rotation-recovery fallthrough: if the
/// newest generation is corrupt, fall back to an older one instead of
/// failing outright. `None` means every candidate was missing or corrupt —
/// the catastrophic path this design accepts (see README).
pub fn load_any_verified(paths: &[PathBuf]) -> Option<IdMapIndex> {
    paths.iter().find_map(|p| load_verified(p).ok())
}
