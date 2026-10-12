use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

/// A private temporary directory for standard input and for output that has to wait for its
/// turn, removed when dropped.
pub struct Spool {
    dir: PathBuf,
}

impl Spool {
    /// Creates the spool inside `parent`, or the system temporary directory.
    pub fn create(parent: Option<&Path>) -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let parent = parent.map_or_else(std::env::temp_dir, Path::to_path_buf);
        let dir = parent.join(format!("extractous-{}-{nanos}", process::id()));
        fs::create_dir(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn copy_stdin(&self) -> io::Result<PathBuf> {
        let path = self.path("stdin");
        let mut file = fs::File::create_new(&path)?;
        io::copy(&mut io::stdin().lock(), &mut file)?;
        file.sync_all()?;
        Ok(path)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.dir) {
            eprintln!(
                "extractous: Failed to remove {}: {error}",
                self.dir.display()
            );
        }
    }
}
