//! Tika's `BPListDetector`: binary property lists, named by the keys of their root dictionary,
//! the whole list checked as dd-plist's `BinaryPropertyListParser` parses it, read in place. A
//! list it rejects makes Tika's detection fail.

use crate::bplist::{self as binary, ListKind, Node};
use crate::input::Pages;
use crate::mime::media_type::MediaType;

use super::Thrown;

/// The binary property list type, `None` for a document that is not one, or what Tika throws
/// for one it cannot parse.
pub(crate) fn detect(data: &[u8]) -> Result<Option<MediaType>, Thrown> {
    if data.len() < 6 || !data.starts_with(b"bplist") {
        return Ok(None);
    }
    let thrown = |thrown| match thrown {
        binary::Thrown::Format(_) => Thrown::Io,
        binary::Thrown::Runtime(_) => Thrown::Runtime,
    };
    let reader = binary::Reader::open(data, Pages::IN_MEMORY).map_err(thrown)?;
    reader.validate().map_err(thrown)?;
    let kind = match reader.node(reader.top()).map_err(thrown)? {
        Node::Dict(pairs) => reader.kind(pairs).map_err(thrown)?,
        _ => ListKind::Plain,
    };
    Ok(Some(MediaType::named(kind.binary_type())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary property list of one dictionary mapping each key to itself.
    fn plist(keys: &[&str]) -> Vec<u8> {
        let mut data = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        offsets.push(data.len());
        data.push(0xD0 | keys.len() as u8);
        for i in 0..keys.len() {
            data.push(i as u8 + 1);
        }
        for i in 0..keys.len() {
            data.push(i as u8 + 1);
        }
        for key in keys {
            offsets.push(data.len());
            if key.len() < 15 {
                data.push(0x50 | key.len() as u8);
            } else {
                data.extend([0x5F, 0x10, key.len() as u8]);
            }
            data.extend(key.as_bytes());
        }
        let table = data.len();
        data.extend(offsets.iter().map(|&o| o as u8));
        let mut trailer = vec![0u8; 6];
        trailer.push(1);
        trailer.push(1);
        trailer.extend((offsets.len() as u64).to_be_bytes());
        trailer.extend(0u64.to_be_bytes());
        trailer.extend((table as u64).to_be_bytes());
        data.extend(trailer);
        data
    }

    #[test]
    fn names_lists_by_their_keys() {
        assert_eq!(
            detect(&plist(&["WebMainResource"]))
                .ok()
                .flatten()
                .unwrap()
                .as_str(),
            "application/x-bplist-webarchive"
        );
        assert_eq!(
            detect(&plist(&["Tracks", "Playlists", "Music Folder"]))
                .ok()
                .flatten()
                .unwrap()
                .as_str(),
            "application/x-bplist-itunes"
        );
        assert_eq!(
            detect(&plist(&["x"])).ok().flatten().unwrap().as_str(),
            "application/x-bplist"
        );
    }

    #[test]
    fn fails_on_lists_it_cannot_parse() {
        let mut broken = plist(&["x"]);
        let at = broken.len() - 9;
        broken[at] = 0xFF;
        assert!(detect(&broken).is_err());
        assert!(detect(b"bplist01").is_err());
        assert!(detect(b"<plist>").unwrap().is_none());
    }
}
