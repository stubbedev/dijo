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

/// How many bodies of `length` bytes make a file too large to read whole.
pub(super) fn bodies_past_limit(length: usize) -> u64 {
    MAX_READ_WHOLE / length as u64 + 1
}

/// A file larger than [`MAX_READ_WHOLE`] in a directory of its own, removed when it is dropped.
pub(super) struct LargeFile {
    dir: PathBuf,
    path: PathBuf,
}

impl LargeFile {
    /// `head`, then the `count` bodies that `body` makes of their indexes, then `tail`, which
    /// must make a file larger than [`MAX_READ_WHOLE`].
    pub(super) fn new(
        name: &str,
        head: &[u8],
        count: u64,
        mut body: impl FnMut(u64) -> Vec<u8>,
        tail: &[u8],
    ) -> Self {
        let dir = crate::isolate::temp_dir().join(format!(
            "extractous-media-large-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(name);
        let mut out = BufWriter::new(File::create(&path).unwrap());
        out.write_all(head).unwrap();
        for index in 0..count {
            out.write_all(&body(index)).unwrap();
        }
        out.write_all(tail).unwrap();
        let file = out.into_inner().unwrap();
        file.sync_all().unwrap();
        assert!(file.metadata().unwrap().len() > MAX_READ_WHOLE);
        Self { dir, path }
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
