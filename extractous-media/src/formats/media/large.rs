//! Files too large to read whole, which the tests write and extract under a memory budget far
//! below their size, so that a parser that declares its memory bounded is held to it.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use extractous_core::Extractor;

use crate::input::MAX_READ_WHOLE;
use crate::{memory, Metadata};

/// The memory an extraction of a large file may take: a fraction of the file.
const BUDGET: u64 = 16 << 20;

/// A file larger than [`MAX_READ_WHOLE`] in a directory of its own, removed when it is dropped.
pub(super) struct LargeFile {
    dir: PathBuf,
    path: PathBuf,
}

impl LargeFile {
    /// `head`, then each of `bodies` that `body` makes of its index, until the file is larger than
    /// [`MAX_READ_WHOLE`], then `tail`; with how many bodies it holds.
    pub(super) fn new(
        name: &str,
        head: &[u8],
        mut body: impl FnMut(u64) -> Vec<u8>,
        tail: &[u8],
    ) -> (Self, u64) {
        let dir = crate::isolate::temp_dir().join(format!(
            "extractous-media-large-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(name);
        let mut out = BufWriter::new(File::create(&path).unwrap());
        out.write_all(head).unwrap();
        let mut written = head.len() as u64;
        let mut bodies = 0;
        while written <= MAX_READ_WHOLE {
            let bytes = body(bodies);
            out.write_all(&bytes).unwrap();
            written += bytes.len() as u64;
            bodies += 1;
        }
        out.write_all(tail).unwrap();
        out.into_inner().unwrap().sync_all().unwrap();
        (Self { dir, path }, bodies)
    }

    /// The file's text and metadata, extracted under [`BUDGET`], which `parser` must have read.
    pub(super) fn extract(&self, parser: &str) -> (String, Metadata) {
        let path = self.path.to_str().unwrap();
        let (text, metadata) =
            memory::within(BUDGET, || Extractor::new().extract_file_to_string(path))
                .unwrap_or_else(|error| panic!("{path} extracts under {BUDGET} bytes: {error}"));
        assert_eq!(metadata["tk:parsed-by"].last().unwrap(), parser);
        (text, metadata)
    }
}

impl Drop for LargeFile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}
