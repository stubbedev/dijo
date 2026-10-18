//! The ZIP archive of a package as Tika's `OpenDocumentParser` and `EpubParser` read it: the
//! `ZipFile` Tika's ZIP detector opened on the document, or, where `ZipFile` cannot open it, on
//! the archive `ZipSalvager` rebuilt from it, as the detection's `zip:salvaged` records. Without
//! either, the parser opens the document itself with `ZipFileHelper.open`, which throws. Its
//! entries are read as `ZipFile.getInputStream` reads them, duplicates and all.

use std::borrow::Cow;
use std::sync::Arc;

use crate::compress::zip::file::{self, ZipEntry, ZipFile};
use crate::compress::zip::salvage::{salvage, Unsalvaged};
use crate::compress::zip::Content;
use crate::compress::MAX_DECODED;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::{Decline, Difference, Failure, Prepared};
use crate::input::{Input, Pages};
use crate::mime::Key;

/// The exception the `TikaInputStream` over an entry wraps what its reading throws in.
const TAGGED_IO_EXCEPTION: &str = "org.apache.commons.io.TaggedIOException";
const UNSUPPORTED_ZIP_FEATURE: &str =
    "org.apache.commons.compress.archivers.zip.UnsupportedZipFeatureException";

/// Whether there is a Tika path to leave a document to.
const DEFERS: bool = cfg!(feature = "tika");

/// The bytes of the archive a parser reads: the document's own, or those salvaged from it.
pub(crate) struct ZipPackage<'a> {
    data: Cow<'a, [u8]>,
    /// The pages of the document's bytes, for a reading of them to release.
    pages: Pages<'a>,
}

impl<'a> ZipPackage<'a> {
    /// The archive Tika's parser `tika_name` reads of `input`, whose pages are `pages` and whose
    /// detection left `metadata`; or the exception it throws where it has none to read.
    pub(crate) fn open(
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        tika_name: &str,
    ) -> Result<Self, Decline<'static>> {
        if ZipFile::open(input).is_ok() {
            return Ok(Self {
                data: Cow::Borrowed(input),
                pages,
            });
        }
        if metadata.get(Key::Salvaged.name()) == Some("true") {
            match salvage(input) {
                Ok(salvaged) => {
                    return Ok(Self {
                        data: Cow::Owned(salvaged),
                        pages: Pages::IN_MEMORY,
                    })
                }
                Err(Unsalvaged::Unported(reason)) => {
                    return Err(Decline::Deferred(format!(
                        "Tika's ZIP detector salvaged its archive, which the port does not \
                         salvage, for {reason}"
                    )))
                }
                Err(Unsalvaged::Thrown) => {}
            }
        }
        if file::has_end_of_central_directory(input) {
            return Err(Decline::Deferred(
                "ZipFile cannot open its archive, which Tika's ZIP detector did not salvage, and \
                 the port does not tell which exception ZipFile throws on it"
                    .to_owned(),
            ));
        }
        Err(Decline::Fails(
            Thrown::Io(Exception::new(
                "java.util.zip.ZipException",
                "Archive is not a ZIP archive",
            ))
            .into_error(tika_name),
        ))
    }

    /// The archive opened, as it opened when the package was.
    pub(crate) fn zip(&self) -> ZipFile<'_> {
        ZipFile::open(&self.data).expect("an archive that opened opens again")
    }

    /// `bytes` read from the archive, as an input: the archive's own, whose reading releases the
    /// pages of the document's bytes, or decompressed.
    pub(crate) fn input<'b>(&self, bytes: Cow<'b, [u8]>) -> Input<'b>
    where
        'a: 'b,
    {
        match bytes {
            Cow::Borrowed(bytes) => Input::Within(bytes, self.pages),
            Cow::Owned(bytes) => Input::Read(Arc::new(bytes)),
        }
    }
}

/// Why the data of an entry is not read as `ZipFile.getInputStream` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unread {
    /// `getInputStream` throws an `UnsupportedZipFeatureException` with this message, which a
    /// native reading throws too.
    Unsupported(String),
    /// The port does not decode it as commons-compress does.
    Undecoded(Undecoded),
}

/// Why the port does not decode the data of an entry as commons-compress does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// Its data is cut short or corrupt: commons-compress throws an `EOFException` or a
    /// `ZipException` with zlib's message, which the port does not tell apart.
    CutShort,
    /// It decodes into more than [`MAX_DECODED`] bytes, which Tika streams.
    Oversized,
}

/// How Tika's parser reads an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opened {
    /// Through a `TikaInputStream` over `ZipFile.getInputStream`, which wraps what its reading
    /// throws in a `TaggedIOException`.
    Tagged,
    /// With `ZipFile.getInputStream` alone.
    Directly,
}

impl Unread {
    /// How a native reading fails on the entry `name`, read as Tika's parser `tika_name` reads it,
    /// `opened` so, where that parser throws.
    pub(crate) fn failure(self, name: &str, opened: Opened, tika_name: &str) -> Failure {
        match self {
            Self::Unsupported(message) => {
                let unsupported = Exception::new(UNSUPPORTED_ZIP_FEATURE, message.clone());
                let thrown = match opened {
                    Opened::Tagged => {
                        Exception::new(TAGGED_IO_EXCEPTION, message).caused_by(unsupported)
                    }
                    Opened::Directly => unsupported,
                };
                Failure::Extraction(Thrown::Io(thrown).into_error(tika_name))
            }
            Self::Undecoded(Undecoded::CutShort) => Failure::Malformed(format!(
                "the data of the entry {name} is cut short or corrupt"
            )),
            Self::Undecoded(Undecoded::Oversized) => {
                Failure::Extraction(Error::LimitExceeded(past_the_limit(name)))
            }
        }
    }
}

impl Undecoded {
    /// How a parser declines a document whose entry `name` it does not decode, where `reading`
    /// is what it writes of the document, failing at that entry.
    pub(crate) fn decline<'p>(self, name: &str, reading: Prepared<'p>) -> Decline<'p> {
        match self {
            Self::CutShort => Decline::Differs {
                reading,
                difference: Difference::new(format!(
                    "the data of the entry {name} is cut short or corrupt: the native parser \
                     fails the document as malformed there, where Tika's path fails it with the \
                     exception its inflater throws"
                )),
            },
            Self::Oversized if DEFERS => Decline::Deferred(format!(
                "{}, which Tika's path streams",
                past_the_limit(name)
            )),
            Self::Oversized => Decline::Fails(Error::LimitExceeded(past_the_limit(name))),
        }
    }
}

fn past_the_limit(name: &str) -> String {
    format!(
        "the entry {name} decodes into more than {MAX_DECODED} bytes, the most the native parser \
         decodes"
    )
}

/// The data of `entry` of `zip`, as `ZipFile.getInputStream` reads it to its end.
pub(crate) fn read<'d>(zip: &ZipFile<'d>, entry: &ZipEntry) -> Result<Cow<'d, [u8]>, Unread> {
    if let Some(message) = entry.unsupported_feature() {
        return Err(Unread::Unsupported(message));
    }
    match zip
        .read(entry)
        .expect("ZipFile reads an entry commons-compress supports")
    {
        Content::Complete(bytes) => Ok(bytes),
        Content::Truncated(_) => Err(Unread::Undecoded(Undecoded::CutShort)),
        Content::Oversized(_) => Err(Unread::Undecoded(Undecoded::Oversized)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// `bytes` with every `from` replaced by `to`, of the same length.
    pub(crate) fn replace_all(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        assert_eq!(from.len(), to.len());
        let mut replaced = bytes.to_vec();
        let mut index = 0;
        while let Some(found) = replaced[index..]
            .windows(from.len())
            .position(|window| window == from)
        {
            replaced[index + found..index + found + from.len()].copy_from_slice(to);
            index += found + from.len();
        }
        replaced
    }

    /// The archive `bytes` with the entry `name` said, in its local and its central header, to be
    /// compressed with `method`.
    pub(crate) fn with_method(mut bytes: Vec<u8>, name: &str, method: u16) -> Vec<u8> {
        let name = name.as_bytes();
        for (signature, method_at, name_at) in [(b"PK\x03\x04", 8, 30), (b"PK\x01\x02", 10, 46)] {
            let header = (0..bytes.len() - name_at - name.len())
                .find(|&at| {
                    bytes[at..at + 4] == *signature
                        && bytes[at + name_at..at + name_at + name.len()] == *name
                })
                .expect("the entry's header");
            bytes[header + method_at..header + method_at + 2]
                .copy_from_slice(&method.to_le_bytes());
        }
        bytes
    }
}
