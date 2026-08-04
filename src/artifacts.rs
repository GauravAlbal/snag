use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub struct ArtifactStorage {
    objects_dir: PathBuf,
}

impl ArtifactStorage {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let objects_dir = data_dir.join("objects").join("blake3");
        fs::create_dir_all(&objects_dir)?;
        Ok(Self { objects_dir })
    }

    /// Safely copy an artifact into the content-addressed store.
    pub fn ingest_file(&self, source: &Path) -> Result<(String, u64)> {
        if !source.is_file() {
            anyhow::bail!("Artifact is not a file: {}", source.display());
        }

        // Check symlinks
        let meta = fs::symlink_metadata(source)?;
        if meta.is_symlink() {
            anyhow::bail!("Artifact is a symlink: {}", source.display());
        }
        
        let mut file = fs::File::open(source)?;
        let mut temp = NamedTempFile::new_in(&self.objects_dir)?;
        
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0; 64 * 1024];
        let mut total_bytes = 0;

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            temp.write_all(&buffer[..n])?;
            total_bytes += n as u64;
            
            if total_bytes > 50 * 1024 * 1024 {
                anyhow::bail!("Artifact exceeds 50 MiB limit: {}", source.display());
            }
        }

        temp.flush()?;
        // Sync before rename for durability
        temp.as_file().sync_all()?;
        
        let hash = hasher.finalize().to_hex().to_string();
        let prefix = &hash[0..2];
        let prefix_dir = self.objects_dir.join(prefix);
        fs::create_dir_all(&prefix_dir)?;
        
        let target_path = prefix_dir.join(&hash);
        
        // Atomically rename. If it already exists, we can ignore or overwrite. 
        // tempfile's persist safely renames.
        if !target_path.exists() {
            temp.persist(&target_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target_path, fs::Permissions::from_mode(0o600)).ok();
            }
        }

        Ok((format!("blake3:{}", hash), total_bytes))
    }
}
