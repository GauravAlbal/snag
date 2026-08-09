use anyhow::Result;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub const MAX_ARTIFACT_COUNT: usize = 64;
pub const MAX_ARTIFACT_BYTES: u64 = 250 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES_PER_FILE: u64 = 50 * 1024 * 1024;

pub struct ArtifactStorage {
    objects_dir: PathBuf,
}

/// Files created while publishing one observation. Objects that already
/// existed are never tracked, so aborting an attempt cannot remove deduped
/// content belonging to another observation.
pub struct ArtifactAttempt<'a> {
    storage: &'a ArtifactStorage,
    created_objects: Vec<PathBuf>,
    created_dirs: Vec<PathBuf>,
    committed: bool,
}

impl Drop for ArtifactAttempt<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in &self.created_objects {
            let _ = fs::remove_file(path);
        }
        for path in self.created_dirs.iter().rev() {
            let _ = fs::remove_dir(path);
        }
    }
}

impl ArtifactStorage {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let objects_root = data_dir.join("objects");
        crate::store::ensure_private_dir(&objects_root)?;
        let objects_dir = objects_root.join("blake3");
        crate::store::ensure_private_dir(&objects_dir)?;
        Ok(Self { objects_dir })
    }

    /// Validate all artifact metadata and declared limits before creating any
    /// object. Streaming ingestion repeats the limits to cover source races.
    pub fn preflight(&self, sources: &[PathBuf]) -> Result<()> {
        if sources.len() > MAX_ARTIFACT_COUNT {
            return Err(crate::error::SnagError::ArtifactInvalid(format!(
                "too many artifacts: {} (maximum {})",
                sources.len(),
                MAX_ARTIFACT_COUNT
            ))
            .into());
        }

        let mut total = 0_u64;
        for source in sources {
            let meta = fs::symlink_metadata(source).map_err(|error| {
                anyhow::Error::from(crate::error::SnagError::ArtifactInvalid(format!(
                    "{}: {error}",
                    source.display()
                )))
            })?;
            if meta.file_type().is_symlink() {
                return Err(crate::error::SnagError::ArtifactInvalid(format!(
                    "Artifact may not be a symlink: {}",
                    source.display()
                ))
                .into());
            }
            if !meta.file_type().is_file() {
                return Err(crate::error::SnagError::ArtifactInvalid(format!(
                    "Artifact is not a regular file: {}",
                    source.display()
                ))
                .into());
            }
            let size = meta.len();
            if size > MAX_ARTIFACT_BYTES_PER_FILE {
                return Err(crate::error::SnagError::ArtifactTooLarge(format!(
                    "{} exceeds 50 MiB limit",
                    source.display()
                ))
                .into());
            }
            total = total.checked_add(size).ok_or_else(|| {
                anyhow::Error::from(crate::error::SnagError::ArtifactTooLarge(
                    "aggregate artifact size overflows".to_string(),
                ))
            })?;
            if total > MAX_ARTIFACT_BYTES {
                return Err(crate::error::SnagError::ArtifactTooLarge(
                    "total artifacts size exceeds 250 MiB limit".to_string(),
                )
                .into());
            }
        }
        Ok(())
    }

    pub fn begin_attempt(&self) -> ArtifactAttempt<'_> {
        ArtifactAttempt {
            storage: self,
            created_objects: Vec::new(),
            created_dirs: Vec::new(),
            committed: false,
        }
    }

    /// Safely copy one artifact into the content-addressed store.
    ///
    /// This compatibility wrapper commits a standalone attempt immediately;
    /// report publication uses `begin_attempt` so its objects remain
    /// deletable until the observation transaction succeeds.
    pub fn ingest_file(&self, source: &Path) -> Result<(String, u64)> {
        let mut attempt = self.begin_attempt();
        let result = attempt.ingest_file(source)?;
        attempt.commit();
        Ok(result)
    }
}

impl ArtifactAttempt<'_> {
    pub fn ingest_file(&mut self, source: &Path) -> Result<(String, u64)> {
        let meta = fs::symlink_metadata(source)?;
        if meta.file_type().is_symlink() {
            return Err(crate::error::SnagError::ArtifactInvalid(format!(
                "Artifact may not be a symlink: {}",
                source.display()
            ))
            .into());
        }
        if !meta.file_type().is_file() {
            return Err(crate::error::SnagError::ArtifactInvalid(format!(
                "Artifact is not a regular file: {}",
                source.display()
            ))
            .into());
        }
        if meta.len() > MAX_ARTIFACT_BYTES_PER_FILE {
            return Err(crate::error::SnagError::ArtifactTooLarge(format!(
                "{} exceeds 50 MiB limit",
                source.display()
            ))
            .into());
        }

        let mut file = fs::File::open(source)?;
        let mut temp = NamedTempFile::new_in(&self.storage.objects_dir)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0; 64 * 1024];
        let mut total_bytes = 0_u64;

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total_bytes = total_bytes
                .checked_add(n as u64)
                .ok_or_else(|| anyhow::anyhow!("artifact size overflow"))?;
            if total_bytes > MAX_ARTIFACT_BYTES_PER_FILE {
                return Err(crate::error::SnagError::ArtifactTooLarge(format!(
                    "{} exceeds 50 MiB limit",
                    source.display()
                ))
                .into());
            }
            hasher.update(&buffer[..n]);
            temp.write_all(&buffer[..n])?;
        }

        temp.flush()?;
        temp.as_file().sync_all()?;

        let hash = hasher.finalize().to_hex().to_string();
        let prefix_dir = self.storage.objects_dir.join(&hash[0..2]);
        if crate::store::ensure_private_child_dir(&prefix_dir)? {
            self.created_dirs.push(prefix_dir.clone());
        }
        let target_path = prefix_dir.join(&hash);

        match fs::symlink_metadata(&target_path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.file_type().is_file() => {
                anyhow::bail!(
                    "Artifact object path is not a regular file: {}",
                    target_path.display()
                );
            }
            Ok(_) => {
                crate::store::ensure_private_file(&target_path)?;
                return Ok((format!("blake3:{hash}"), total_bytes));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        match temp.persist_noclobber(&target_path) {
            Ok(_) => {
                self.created_objects.push(target_path.clone());
                crate::store::ensure_private_file(&target_path)?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another writer won the race; its object must not be removed.
            }
            Err(error) => return Err(error.into()),
        }

        Ok((format!("blake3:{hash}"), total_bytes))
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}
