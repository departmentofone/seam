//! Records every file an import creates or changes, keeping the original
//! bytes, so the whole import can be undone.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Log {
    created: Vec<PathBuf>,
    /// (original location, saved copy)
    replaced: Vec<(PathBuf, PathBuf)>,
}

pub struct Journal {
    dir: PathBuf,
    log: Log,
    dry: bool,
}

impl Journal {
    pub fn new(dir: PathBuf, dry: bool) -> anyhow::Result<Self> {
        if !dry {
            std::fs::create_dir_all(dir.join("files"))?;
        }
        Ok(Self { dir, log: Log::default(), dry })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write `bytes` to `path`, saving whatever was there first.
    pub fn write(&mut self, path: &Path, bytes: &[u8], modified: Option<std::time::SystemTime>) -> anyhow::Result<()> {
        use anyhow::Context;
        self.write_inner(path, bytes, modified).with_context(|| format!("writing {}", path.display()))
    }

    fn write_inner(&mut self, path: &Path, bytes: &[u8], modified: Option<std::time::SystemTime>) -> anyhow::Result<()> {
        if self.dry {
            return Ok(());
        }
        self.before_change(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        if let Some(t) = modified {
            if let Ok(f) = std::fs::File::options().write(true).open(path) {
                let _ = f.set_modified(t);
            }
        }
        Ok(())
    }

    /// Copy a file into place, saving whatever was there first.
    pub fn copy(&mut self, from: &Path, to: &Path) -> anyhow::Result<()> {
        use anyhow::Context;
        if self.dry {
            return Ok(());
        }
        let run = |j: &mut Self| -> anyhow::Result<()> {
            j.before_change(to)?;
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(from, to)?;
            Ok(())
        };
        run(self).with_context(|| format!("copying to {}", to.display()))
    }

    fn before_change(&mut self, path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            if self.log.replaced.iter().any(|(p, _)| p == path) || self.log.created.iter().any(|p| p == path) {
                return Ok(());
            }
            let saved = self.dir.join("files").join(format!("{:06}", self.log.replaced.len()));
            std::fs::copy(path, &saved)?;
            self.log.replaced.push((path.to_path_buf(), saved));
        } else {
            self.log.created.push(path.to_path_buf());
        }
        Ok(())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if self.dry {
            return Ok(());
        }
        std::fs::write(self.dir.join("journal.json"), serde_json::to_vec_pretty(&self.log)?)?;
        Ok(())
    }

    pub fn changed(&self) -> usize {
        self.log.created.len() + self.log.replaced.len()
    }
}

/// Put everything back the way it was before the import in `dir`.
pub fn undo(dir: &Path) -> anyhow::Result<usize> {
    let log: Log = serde_json::from_slice(&std::fs::read(dir.join("journal.json"))?)?;
    let mut n = 0;
    for p in log.created.iter().rev() {
        if std::fs::remove_file(p).is_ok() {
            n += 1;
        }
        // Remove folders the import created, if they are now empty.
        let mut d = p.parent();
        while let Some(dir) = d {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
            d = dir.parent();
        }
    }
    for (orig, saved) in &log.replaced {
        if std::fs::copy(saved, orig).is_ok() {
            n += 1;
        }
    }
    std::fs::rename(dir.join("journal.json"), dir.join("journal.undone.json"))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_restores_and_removes() {
        let root = std::env::temp_dir().join(format!("seam-journal-{}", rand::random::<u32>()));
        let target = root.join("t");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), b"original").unwrap();
        let mut j = Journal::new(root.join("backup"), false).unwrap();
        j.write(&target.join("keep.txt"), b"changed", None).unwrap();
        j.write(&target.join("keep.txt"), b"changed twice", None).unwrap();
        j.write(&target.join("new").join("file.txt"), b"new", None).unwrap();
        j.save().unwrap();
        assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"changed twice");
        undo(&root.join("backup")).unwrap();
        assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"original");
        assert!(!target.join("new").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
