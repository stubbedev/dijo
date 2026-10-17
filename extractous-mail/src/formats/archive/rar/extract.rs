//! junrar's `Archive.getInputStream`, `extractFile` and `doExtractFile`, as Tika's parser
//! calls them for each file of an archive in turn: an entry of a solid archive continues the
//! unpacker's state from the last one extracted, decoding any skipped in between, and an
//! entry whose extraction throws gives the data written before, which the port gives only
//! where junrar throws before it writes anything.
//!
//! junrar extracts each entry on a thread of its own into a pipe of 32 KiB, and the entry's
//! extraction counts as done only once the thread has written all of it. Where Tika's parse of
//! an entry larger than the pipe stops reading before its end, the thread fails, and the next
//! entry of a solid stream decodes the skipped data again, on top of the state the failed
//! thread left, while that thread may still run. Whether the parse reads to the end depends on
//! the parser, so the port leaves such archives to Tika. Where the next entry is not solid,
//! only its PPMd model can be cut short, by the failed thread's clean-up, if that thread wakes
//! up while the entry is being decoded; the port gives the entry as decoded.

use super::headers::{FileHeader, Headers};
use super::unpack::{Sink, Thrown, Unpack};
use crate::formats::Deferral;

/// `PIPE_BUFFER_SIZE`: the bytes junrar's pipe holds.
const PIPE_BUFFER_SIZE: usize = 32 * 1024;

/// Why an extraction does not give an entry's data.
enum Failure {
    /// junrar throws before it writes anything, as it does on a broken header or an
    /// encrypted entry, which it has no password for: the entry's stream is empty.
    Refused,
    /// The unpacker throws, or the data does not match its CRC, where junrar's stream ends
    /// early or the port may have decoded what junrar does not.
    Unported(String),
}

impl From<Thrown> for Failure {
    fn from(Thrown(exception): Thrown) -> Self {
        Failure::Unported(format!("junrar throws {exception} unpacking an entry"))
    }
}

/// The data the stream of each file of the archive gives, in order, but of its directories,
/// which Tika's parser does not open.
pub(super) fn streams(data: &[u8], headers: &Headers) -> Result<Vec<(usize, Vec<u8>)>, Deferral> {
    let mut extractor = Extractor {
        unpack: Unpack::new(data),
        solid_archive: headers.solid,
        next_index: 0,
    };
    let mut streams = Vec::new();
    let mut previous_written = 0;
    for (index, header) in headers.files.iter().enumerate() {
        if header.is_directory() {
            continue;
        }
        if header.full_unpack_size <= 0 {
            streams.push((index, Vec::new()));
            continue;
        }
        if previous_written > PIPE_BUFFER_SIZE && extractor.solid_stream(header) {
            return Err(format!(
                "a solid archive whose entry of {previous_written} bytes, more than junrar's \
                 pipe of {PIPE_BUFFER_SIZE} holds, comes before another: the later entry's data \
                 is junrar's only where Tika's parse of the earlier one reads it to its end"
            ));
        }
        let data = match extractor.extract_file(&headers.files, index) {
            Ok(data) => data,
            Err(Failure::Refused) => Vec::new(),
            Err(Failure::Unported(reason)) => return Err(reason),
        };
        previous_written = data.len();
        streams.push((index, data));
    }
    Ok(streams)
}

struct Extractor<'a> {
    unpack: Unpack<'a>,
    /// `MainHeader.isSolid()`.
    solid_archive: bool,
    /// The index after `lastProcessedFileIndex`: the first file a solid stream has not
    /// decoded.
    next_index: usize,
}

impl Extractor<'_> {
    fn solid_stream(&self, header: &FileHeader) -> bool {
        self.solid_archive || header.is_solid()
    }

    /// `extractFile`: the files a solid stream skipped decoded, then the file's data.
    fn extract_file(&mut self, files: &[FileHeader], index: usize) -> Result<Vec<u8>, Failure> {
        let header = &files[index];
        if self.solid_stream(header) {
            for skipped in &files[self.next_index..index] {
                self.skip_file(skipped)?;
            }
        }
        let sink = self.do_extract_file(header, Sink::Keep(Vec::new()), false)?;
        self.next_index = index + 1;
        match sink {
            Sink::Keep(data) => Ok(data),
            Sink::Discard => Ok(Vec::new()),
        }
    }

    /// `skipFile`: a file's data decoded into nothing, a refusal ignored.
    fn skip_file(&mut self, header: &FileHeader) -> Result<(), Failure> {
        if header.full_unpack_size <= 0 {
            return Ok(());
        }
        match self.do_extract_file(header, Sink::Discard, true) {
            Ok(_) | Err(Failure::Refused) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    /// `doExtractFile`: the file's data unpacked into `sink`, checked against its CRC unless
    /// it is skipped.
    fn do_extract_file(
        &mut self,
        header: &FileHeader,
        sink: Sink,
        skip: bool,
    ) -> Result<Sink, Failure> {
        if header.broken {
            return Err(Failure::Refused);
        }
        let start = u64::try_from(header.data_start()).map_err(|_| Failure::Refused)?;
        self.unpack.start_entry(sink, start, header.full_pack_size);
        if header.is_encrypted() {
            return Err(Failure::Refused);
        }
        if !header.is_solid() {
            self.unpack.init();
        }
        self.unpack.set_dest_size(header.full_unpack_size);
        self.unpack
            .do_unpack(header.unp_method, header.unp_version, header.is_solid())?;
        if !skip && self.unpack.crc() != header.file_crc {
            return Err(Failure::Unported(
                "an entry whose unpacked data does not match its CRC, where junrar's stream \
                 ends with the error"
                    .to_owned(),
            ));
        }
        Ok(self.unpack.take_sink())
    }
}
