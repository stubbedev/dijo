//! BitTorrent metainfo files (`.torrent`), which Tika does not read, read for what they describe:
//! the name of the torrent as a heading, its comment, its files in a table of their paths and
//! sizes in bytes, and its trackers' announce URLs in a list. The name is the `dc:title`, the
//! comment the `dc:description`, the client that created it (`created by`) the
//! `xmp:CreatorTool`, its creation date, seconds since the Unix epoch, the `dcterms:created`, and
//! each announce URL, of `announce` and of the tiers of `announce-list` without repeats, a
//! `bittorrent:announce`.
//!
//! The files are those of a version 1 torrent's `files`, of a version 2 torrent's `file tree`, or
//! the one file a single-file torrent names; a hybrid torrent's are its `files`. A file's path is
//! its components joined by `/`, and padding files (BEP 47), there to align pieces, are not
//! written. The piece hashes are not read. Text is decoded as UTF-8, or in the charset the torrent's
//! `encoding` names, and a `name.utf-8`, `path.utf-8` or `comment.utf-8` some clients write beside
//! a field in another charset is taken over it.
//!
//! A torrent is bencoded ([`bencode`]), which is checked whole, its nesting and lengths bounded,
//! before anything is written.

mod bencode;

use std::collections::HashSet;

use bencode::{Dictionary, List, Malformed, Value};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java::time::instant_string;
use crate::text::charset::Charset;

const NAME: &str = "dijo::BitTorrentParser";
const WHAT: &str = "The torrent";
const ANNOUNCE: &str = "bittorrent:announce";
/// How BitComet names the padding files of torrents written before BEP 47 marked them.
const PADDING_PREFIX: &[u8] = b"_____padding_file_";

/// The text of BitTorrent metainfo files.
pub struct BitTorrentParser;

impl Parser for BitTorrentParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-bittorrent"]
    }

    /// Fails a file that is not a bencoded torrent, or whose fields are not of their types, before
    /// anything is written.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let torrent = Torrent::read(input)?;
        Ok(Box::new(move |xhtml| torrent.write(xhtml)))
    }
}

/// Where a torrent's fields are not of their types, as its reading fails.
#[derive(Debug)]
struct Invalid(String);

impl From<Invalid> for Failure {
    fn from(Invalid(reason): Invalid) -> Self {
        Failure::Malformed(reason)
    }
}

/// The charset a torrent's text is decoded in.
#[derive(Debug, Clone, Copy)]
struct Text(Charset);

impl Text {
    fn utf8() -> Self {
        Self(Charset::named("UTF-8"))
    }

    /// The charset the `encoding` of `root` names, else UTF-8.
    fn of(root: Dictionary<'_>) -> Result<Self, Decline<'static>> {
        let Some(label) = bytes_field(root, "encoding").map_err(invalid)? else {
            return Ok(Self::utf8());
        };
        let label = String::from_utf8_lossy(label);
        match Charset::for_name(label.trim()) {
            Some(charset) if charset.decodes() => Ok(Self(charset)),
            Some(_) | None => Err(Decline::Unread(format!(
                "a torrent whose text is in the charset {label:?}, which is not decoded"
            ))),
        }
    }

    fn decode(self, bytes: &[u8]) -> String {
        self.0
            .decode(bytes)
            .expect("the charset of a torrent's text decodes")
    }
}

/// How a torrent lists its files.
#[derive(Debug, Clone, Copy)]
enum Files<'a> {
    /// The one file of a single-file torrent, of this length, named by the torrent's name.
    Single(u64),
    /// A version 1 torrent's `files`.
    List(List<'a>),
    /// A version 2 torrent's `file tree`.
    Tree(Dictionary<'a>),
}

/// A torrent's fields, of their types, read in place.
#[derive(Debug)]
struct Torrent<'a> {
    text: Text,
    name: String,
    comment: Option<String>,
    created_by: Option<String>,
    created: Option<i64>,
    announce: Vec<String>,
    files: Files<'a>,
}

/// A failure of the reading of a torrent before anything is written.
fn invalid(Invalid(reason): Invalid) -> Decline<'static> {
    Decline::Fails(Error::ParseError(reason))
}

/// `what` of the wrong kind `found`.
fn mistyped(what: &str, expected: &str, found: Value<'_>) -> Invalid {
    Invalid(format!(
        "{WHAT}'s {what} is {}, not {expected}",
        found.kind()
    ))
}

/// The byte string `key` of `dictionary`, if it has one.
fn bytes_field<'a>(dictionary: Dictionary<'a>, key: &str) -> Result<Option<&'a [u8]>, Invalid> {
    match dictionary.get(key) {
        None => Ok(None),
        Some(Value::Bytes(bytes)) => Ok(Some(bytes)),
        Some(other) => Err(mistyped(key, "a byte string", other)),
    }
}

/// The text `key` of `dictionary`, its `key.utf-8` in UTF-8 if it has one, else `key` in `text`.
fn text_field(
    dictionary: Dictionary<'_>,
    key: &str,
    text: Text,
) -> Result<Option<String>, Invalid> {
    if let Some(utf8) = bytes_field(dictionary, &format!("{key}.utf-8"))? {
        return Ok(Some(Text::utf8().decode(utf8)));
    }
    Ok(bytes_field(dictionary, key)?.map(|bytes| text.decode(bytes)))
}

/// The `length` of the file `dictionary`.
fn length(dictionary: Dictionary<'_>) -> Result<u64, Invalid> {
    match dictionary.get("length") {
        Some(Value::Integer(length)) => u64::try_from(length)
            .map_err(|_| Invalid(format!("{WHAT} has a file of the length {length}"))),
        Some(other) => Err(mistyped("file length", "an integer", other)),
        None => Err(Invalid(format!("{WHAT} has a file without a length"))),
    }
}

/// Whether the file of `entry` pads pieces rather than holding data, by its BEP 47 `attr`.
fn is_padding(entry: Dictionary<'_>) -> Result<bool, Invalid> {
    Ok(bytes_field(entry, "attr")?.is_some_and(|attr| attr.contains(&b'p')))
}

impl<'a> Torrent<'a> {
    fn read(input: &'a [u8]) -> Result<Self, Decline<'a>> {
        let root = match bencode::parse(input) {
            Ok(Value::Dictionary(root)) => root,
            Ok(other) => {
                return Err(invalid(Invalid(format!(
                    "{WHAT} is {}, not a dictionary",
                    other.kind()
                ))))
            }
            Err(Malformed(reason)) => {
                return Err(invalid(Invalid(format!(
                    "{WHAT} is not bencoded: {reason}"
                ))))
            }
        };
        let text = Text::of(root)?;
        let info = match root.get("info") {
            Some(Value::Dictionary(info)) => info,
            Some(other) => return Err(invalid(mistyped("info", "a dictionary", other))),
            None => return Err(invalid(Invalid(format!("{WHAT} has no info dictionary")))),
        };
        let torrent = Torrent {
            text,
            name: text_field(info, "name", text)
                .and_then(|name| name.ok_or_else(|| Invalid(format!("{WHAT} has no name"))))
                .map_err(invalid)?,
            comment: text_field(root, "comment", text).map_err(invalid)?,
            created_by: text_field(root, "created by", text).map_err(invalid)?,
            created: match root.get("creation date") {
                None => None,
                Some(Value::Integer(seconds)) => Some(seconds),
                Some(other) => return Err(invalid(mistyped("creation date", "an integer", other))),
            },
            announce: announce(root, text).map_err(invalid)?,
            files: files(info).map_err(invalid)?,
        };
        torrent
            .each_file(|_, _| Ok::<(), Invalid>(()))
            .map_err(invalid)?;
        Ok(torrent)
    }

    /// Hands `visit` the path and the length of each file that is not padding, in order.
    fn each_file<E: From<Invalid>>(
        &self,
        mut visit: impl FnMut(&str, u64) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut path = String::new();
        match self.files {
            Files::Single(length) => visit(&self.name, length),
            Files::List(files) => {
                for file in files.iter() {
                    let Value::Dictionary(file) = file else {
                        return Err(mistyped("file", "a dictionary", file).into());
                    };
                    if is_padding(file)? || !self.list_path(file, &mut path)? {
                        continue;
                    }
                    visit(&path, length(file)?)?;
                }
                Ok(())
            }
            Files::Tree(tree) => self.each_in_tree(tree, &mut path, &mut visit),
        }
    }

    /// Sets `path` to the path of the version 1 `file`; whether it is not a padding file by its
    /// name.
    fn list_path(&self, file: Dictionary<'_>, path: &mut String) -> Result<bool, Invalid> {
        let (components, text) = match (file.get("path.utf-8"), file.get("path")) {
            (Some(components), _) => (components, Text::utf8()),
            (None, Some(components)) => (components, self.text),
            (None, None) => return Err(Invalid(format!("{WHAT} has a file without a path"))),
        };
        let Value::List(components) = components else {
            return Err(mistyped("file path", "a list", components));
        };
        path.clear();
        let mut last: &[u8] = &[];
        for component in components.iter() {
            let Value::Bytes(component) = component else {
                return Err(mistyped("file path component", "a byte string", component));
            };
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&text.decode(component));
            last = component;
        }
        Ok(!last.starts_with(PADDING_PREFIX))
    }

    /// [`Torrent::each_file`] of the directory `tree` of a version 2 file tree at `path`.
    fn each_in_tree<E: From<Invalid>>(
        &self,
        tree: Dictionary<'_>,
        path: &mut String,
        visit: &mut impl FnMut(&str, u64) -> Result<(), E>,
    ) -> Result<(), E> {
        for (name, entry) in tree.entries() {
            let Value::Dictionary(entry) = entry else {
                return Err(mistyped("file tree entry", "a dictionary", entry).into());
            };
            let parent = path.len();
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&self.text.decode(name));
            match entry.get("") {
                Some(Value::Dictionary(file)) => {
                    if !is_padding(file)? {
                        visit(path, length(file)?)?;
                    }
                }
                Some(other) => return Err(mistyped("file", "a dictionary", other).into()),
                None => self.each_in_tree(entry, path, visit)?,
            }
            path.truncate(parent);
        }
        Ok(())
    }

    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        metadata.set("dc:title", self.name.as_str());
        if let Some(comment) = &self.comment {
            metadata.set("dc:description", comment.as_str());
        }
        if let Some(created_by) = &self.created_by {
            metadata.set("xmp:CreatorTool", created_by.as_str());
        }
        if let Some(seconds) = self.created {
            metadata.set("dcterms:created", instant_string(seconds, 0));
        }
        for url in &self.announce {
            metadata.add(ANNOUNCE, url.as_str());
        }
        xhtml.element("h1", &self.name)?;
        if let Some(comment) = &self.comment {
            xhtml.element("p", comment)?;
        }
        xhtml.start_element("table")?;
        xhtml.start_element("tbody")?;
        self.each_file(|path, length| -> Result<(), Failure> {
            xhtml.start_element("tr")?;
            xhtml.element("td", path)?;
            xhtml.element("td", &length.to_string())?;
            xhtml.end_element("tr")?;
            Ok(())
        })?;
        xhtml.end_element("tbody")?;
        xhtml.end_element("table")?;
        if self.announce.is_empty() {
            return Ok(());
        }
        xhtml.start_element("ul")?;
        for url in &self.announce {
            xhtml.element("li", url)?;
        }
        xhtml.end_element("ul")?;
        Ok(())
    }
}

/// How the `info` dictionary lists the torrent's files.
fn files(info: Dictionary<'_>) -> Result<Files<'_>, Invalid> {
    match (info.get("files"), info.get("file tree")) {
        (Some(Value::List(files)), _) => Ok(Files::List(files)),
        (Some(other), _) => Err(mistyped("files", "a list", other)),
        (None, Some(Value::Dictionary(tree))) => Ok(Files::Tree(tree)),
        (None, Some(other)) => Err(mistyped("file tree", "a dictionary", other)),
        (None, None) => Ok(Files::Single(length(info)?)),
    }
}

/// The announce URLs of `root`, `announce` then each of `announce-list`'s tiers, without
/// repeats.
fn announce<'a>(root: Dictionary<'a>, text: Text) -> Result<Vec<String>, Invalid> {
    let mut seen: HashSet<&'a [u8]> = HashSet::new();
    let mut urls = Vec::new();
    let mut take = |url: &'a [u8]| {
        if !url.is_empty() && seen.insert(url) {
            urls.push(text.decode(url));
        }
    };
    if let Some(url) = bytes_field(root, "announce")? {
        take(url);
    }
    match root.get("announce-list") {
        None => {}
        Some(Value::List(tiers)) => {
            for tier in tiers.iter() {
                let Value::List(tier) = tier else {
                    return Err(mistyped("announce-list tier", "a list", tier));
                };
                for url in tier.iter() {
                    let Value::Bytes(url) = url else {
                        return Err(mistyped("announce URL", "a byte string", url));
                    };
                    take(url);
                }
            }
        }
        Some(other) => return Err(mistyped("announce-list", "a list", other)),
    }
    Ok(urls)
}

#[cfg(test)]
mod tests;
