//! What ROME 2.1.0's `SyndFeedInput` makes of a feed, as far as Tika's `FeedParser` reads it:
//! the wire-feed parser its `FeedParsers` picks for the document, and what the converter for
//! that feed type copies into the `SyndFeed` of the feed's title and description and each
//! entry's link, title and description. Where ROME throws, so does [`read`].
//!
//! ROME reads the whole feed before Tika's parser writes, so the feed is read twice: once into
//! its tree without its entries, each checked as it ends where ROME's reading of it may throw,
//! and again, each entry converted as it ends and handed on ([`each_entry`]).

use super::jdom::{self, Element, Node};
use crate::java;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RSS_090: &str = "http://my.netscape.com/rdf/simple/0.9/";
const RSS_10: &str = "http://purl.org/rss/1.0/";
const RSS_20: &str = "http://backend.userland.com/rss2";
const ATOM_10: &str = "http://www.w3.org/2005/Atom";
const ATOM_03: &str = "http://purl.org/atom/ns#";
const SYNDICATION: &str = "http://purl.org/rss/1.0/modules/syndication/";
/// `SyModuleImpl`'s update periods.
const PERIODS: [&str; 5] = ["hourly", "daily", "weekly", "monthly", "yearly"];

/// Why ROME throws on a feed.
pub(super) type Thrown = String;

/// The part of a `SyndFeed` Tika's `FeedParser` reads but its entries, and the wire-feed parser
/// that reads them.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SyndFeed {
    /// The value of `getTitleEx()`, `None` where it or its value is null.
    pub(super) title: Option<String>,
    /// The value of `getDescriptionEx()`, `None` where it or its value is null.
    pub(super) description: Option<String>,
    wire: Wire,
}

/// The part of a `SyndEntry` Tika's `FeedParser` reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SyndEntry {
    pub(super) link: Option<String>,
    /// The value of `getTitleEx()`, `None` where it or its value is null.
    pub(super) title: Option<String>,
    /// The value of `getDescription()`, `None` where there is none; ROME never leaves a
    /// description without a value.
    pub(super) description: Option<String>,
}

/// The wire-feed parsers of ROME's `rome.properties`, in the order `FeedParsers` asks them
/// whether a document is theirs. `RSS091NetscapeParser`, which wants a document type
/// declaration, never takes a document, as ROME refuses those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Rss090,
    Rss091Userland,
    Rss092,
    Rss093,
    Rss094,
    Rss10,
    Rss20WithNamespace,
    Rss20,
    Atom10,
    Atom03,
}

impl Wire {
    /// The parser `FeedParsers` picks for the feed of `root`.
    fn of(root: &Element) -> Option<Self> {
        Self::ALL.into_iter().find(|wire| wire.takes(root))
    }

    const ALL: [Self; 10] = [
        Self::Rss090,
        Self::Rss091Userland,
        Self::Rss092,
        Self::Rss093,
        Self::Rss094,
        Self::Rss10,
        Self::Rss20WithNamespace,
        Self::Rss20,
        Self::Atom10,
        Self::Atom03,
    ];

    /// `isMyType`.
    fn takes(self, root: &Element) -> bool {
        let version = root.attribute("version", "");
        let rss_version = |wanted: &str| root.name == "rss" && version == Some(wanted);
        let rss_20 = || {
            root.name == "rss"
                && version.is_none_or(|version| java::trim(version).starts_with("2.0"))
        };
        match self {
            Self::Rss090 => {
                root.namespace_uri() == RDF
                    && root
                        .additional_namespaces
                        .iter()
                        .any(|namespace| namespace.uri == RSS_090)
            }
            Self::Rss091Userland => rss_version("0.91"),
            Self::Rss092 => rss_version("0.92"),
            Self::Rss093 => rss_version("0.93"),
            Self::Rss094 => rss_version("0.94"),
            Self::Rss10 => root.namespace_uri() == RDF && root.child("channel", RSS_10).is_some(),
            Self::Rss20WithNamespace => root.namespace_uri() == RSS_20 && rss_20(),
            Self::Rss20 => rss_20(),
            Self::Atom10 => root.namespace_uri() == ATOM_10,
            Self::Atom03 => root.namespace_uri() == ATOM_03,
        }
    }

    /// The namespace of the feed's own elements.
    fn namespace(self) -> &'static str {
        match self {
            Self::Rss090 => RSS_090,
            Self::Rss10 => RSS_10,
            Self::Rss20WithNamespace => RSS_20,
            Self::Atom10 => ATOM_10,
            Self::Atom03 => ATOM_03,
            Self::Rss091Userland | Self::Rss092 | Self::Rss093 | Self::Rss094 | Self::Rss20 => "",
        }
    }

    /// Whether the parser is `RSS091UserlandParser` or one derived from it.
    fn is_userland(self) -> bool {
        matches!(
            self,
            Self::Rss091Userland
                | Self::Rss092
                | Self::Rss093
                | Self::Rss094
                | Self::Rss20WithNamespace
                | Self::Rss20
        )
    }

    /// Whether the parser is `RSS092Parser` or one derived from it.
    fn is_092(self) -> bool {
        self.is_userland() && self != Self::Rss091Userland
    }

    /// Whether the feed's converter is `ConverterForRSS094` or one derived from it.
    fn converts_guids(self) -> bool {
        matches!(self, Self::Rss094 | Self::Rss20WithNamespace | Self::Rss20)
    }
}

/// Whether `element`, under `ancestors`, may be an entry of a feed of any type: an item or an
/// entry of the root, or an item of a channel of the root.
fn may_be_entry(ancestors: &[Element], element: &Element) -> bool {
    match ancestors {
        [_] => matches!(element.name.as_str(), "item" | "entry"),
        [_, channel] => channel.name == "channel" && element.name == "item",
        _ => false,
    }
}

/// `new SyndFeedInput().build(input)` with doctypes disallowed: the feed but its entries, or
/// what ROME, JDOM or Xerces throws.
pub(super) fn read(input: &[u8]) -> Result<SyndFeed, Thrown> {
    let mut entry_thrown = None;
    let root = jdom::build(input, &mut may_be_entry, &mut |ancestors, element| {
        let atom_03 = ancestors.len() == 1 && Wire::of(&ancestors[0]) == Some(Wire::Atom03);
        if atom_03 && element.name == "entry" && element.namespace_uri() == ATOM_03 {
            if let Err(thrown) = atom_03_entry(&element) {
                entry_thrown.get_or_insert(thrown);
            }
        }
    })
    .map_err(|reason| format!("Invalid XML: {reason}"))?;
    let wire =
        Wire::of(&root).ok_or_else(|| "IllegalArgumentException: Invalid document".to_owned())?;
    let feed = match wire {
        Wire::Atom10 => atom_10(&root),
        Wire::Atom03 => atom_03(&root),
        wire => rss(&root, wire),
    }?;
    entry_thrown.map_or(Ok(feed), Err)
}

/// Why handing on the entries of a feed stops: its XML is read otherwise the second time, or
/// what takes the entries fails.
pub(super) enum Halt<E> {
    Reread(String),
    Taken(E),
}

/// Reads the feed `input`, which [`read`] read as `feed`, again, handing `each` its entries as
/// ROME converts them, one at a time.
pub(super) fn each_entry<E>(
    input: &[u8],
    feed: &SyndFeed,
    each: &mut dyn FnMut(SyndEntry) -> Result<(), E>,
) -> Result<(), Halt<E>> {
    let wire = feed.wire;
    let namespace = wire.namespace();
    let mut channels = 0usize;
    let mut picks = |ancestors: &[Element], element: &Element| match ancestors {
        [_] if element.namespace_uri() != namespace => false,
        [_] if element.name == "channel" => {
            channels += 1;
            false
        }
        [_] => match wire {
            Wire::Atom10 | Wire::Atom03 => element.name == "entry",
            Wire::Rss090 | Wire::Rss10 => element.name == "item",
            _ => false,
        },
        [_, channel] => {
            wire.is_userland()
                && channels == 1
                && channel.name == "channel"
                && channel.namespace_uri() == namespace
                && element.name == "item"
                && element.namespace_uri() == namespace
        }
        _ => false,
    };
    let mut taken = None;
    jdom::build(input, &mut picks, &mut |_, element| {
        if taken.is_some() {
            return;
        }
        let entry = match wire {
            Wire::Atom10 => Ok(atom_10_entry(&element)),
            Wire::Atom03 => atom_03_entry(&element),
            wire => Ok(rss_entry(&element, wire)),
        };
        taken = Some(match entry {
            Ok(entry) => each(entry).map_err(Halt::Taken),
            Err(thrown) => Err(Halt::Reread(thrown)),
        })
        .filter(Result::is_err);
    })
    .map_err(Halt::Reread)?;
    taken.unwrap_or(Ok(()))
}

fn null_pointer() -> Thrown {
    "NullPointerException".to_owned()
}

/// `Integer.parseInt(text)`, failing as Java does.
fn parse_int(text: &str) -> Result<i32, Thrown> {
    java::parse_int(text)
        .ok_or_else(|| format!("NumberFormatException: For input string: \"{text}\""))
}

/// `SyModuleParser.parse` of `element`, for what it throws: an update period ROME does not know
/// or an update frequency that is no integer.
fn check_syndication(element: &Element) -> Result<(), Thrown> {
    if let Some(period) = element.child("updatePeriod", SYNDICATION) {
        let period = period.text();
        let period = java::trim(&period);
        if !PERIODS.contains(&period) {
            return Err(format!(
                "IllegalArgumentException: Invalid period [{period}]"
            ));
        }
    }
    if let Some(frequency) = element.child("updateFrequency", SYNDICATION) {
        parse_int(java::trim(&frequency.text()))?;
    }
    Ok(())
}

/// The RSS parsers' `parseChannel` and the RSS converters' `copyInto`.
fn rss(root: &Element, wire: Wire) -> Result<SyndFeed, Thrown> {
    let namespace = wire.namespace();
    let channel = root.child("channel", namespace).ok_or_else(null_pointer)?;
    if wire == Wire::Rss10 {
        check_syndication(root)?;
        check_syndication(channel)?;
    }
    if wire.is_userland() {
        if let Some(skip_hours) = channel.child("skipHours", "") {
            for hour in skip_hours.children_named("hour", namespace) {
                parse_int(java::trim(&hour.text()))?;
            }
        }
    }
    if wire.is_092() {
        if let Some(port) = channel
            .child("cloud", namespace)
            .and_then(|cloud| cloud.attribute("port", ""))
        {
            parse_int(java::trim(port))?;
        }
    }
    Ok(SyndFeed {
        title: channel.child("title", namespace).map(Element::text),
        description: channel.child("description", namespace).map(Element::text),
        wire,
    })
}

/// An RSS parser's `parseItem` and its converter's `createSyndEntry`.
fn rss_entry(item: &Element, wire: Wire) -> SyndEntry {
    let namespace = wire.namespace();
    let mut link = item.child("link", namespace).map(Element::text);
    let description = item
        .child("description", namespace)
        .and_then(|description| {
            if wire.is_092() {
                Some(rss_092_description(description))
            } else if wire.is_userland() || wire == Wire::Rss10 {
                Some(description.text())
            } else {
                None
            }
        });
    if wire.converts_guids() && link.is_none() {
        if let Some(guid) = item.child("guid", namespace) {
            let perma_link = guid
                .attribute("isPermaLink", "")
                .is_none_or(|perma_link| java::equals_ignore_case(perma_link, "true"));
            if perma_link {
                link = Some(guid.text());
            }
        }
    }
    SyndEntry {
        link,
        title: item.child("title", namespace).map(Element::text),
        description,
    }
}

/// `RSS092Parser.parseItemDescription`'s value: text and CDATA as they are, child elements as
/// `XMLOutputter` writes them, comments and processing instructions left out.
fn rss_092_description(description: &Element) -> String {
    let mut value = String::new();
    for node in &description.content {
        match node {
            Node::Text(text) | Node::CData(text) => value.push_str(text),
            Node::Element(element) => jdom::output_element(element, &mut value),
            Node::Comment(_) | Node::Instruction { .. } => {}
        }
    }
    value
}

/// `getAttributeValue` of `BaseWireFeedParser`: the attribute without a namespace, else in the
/// feed's.
fn feed_attribute<'e>(element: &'e Element, name: &str, namespace: &str) -> Option<&'e str> {
    element
        .attribute(name, "")
        .or_else(|| element.attribute(name, namespace))
}

/// `Atom10Parser.parseTextConstructToString`: markup content written out, other content as
/// its text.
fn atom_10_text(element: &Element) -> String {
    let kind = feed_attribute(element, "type", ATOM_10).unwrap_or("text");
    if kind == "xhtml" || kind.contains("/xml") || kind.contains("+xml") {
        let mut value = String::new();
        jdom::output(&element.content, &mut value);
        value
    } else {
        element.text()
    }
}

/// `Atom10Parser.parseFeed` and `ConverterForAtom10.copyInto`.
fn atom_10(feed: &Element) -> Result<SyndFeed, Thrown> {
    for link in feed.children_named("link", ATOM_10) {
        if feed_attribute(link, "rel", ATOM_10) == Some("self") {
            feed_attribute(link, "href", ATOM_10)
                .ok_or_else(|| "FeedException: ERROR while finding base URI of feed".to_owned())?;
            break;
        }
    }
    check_syndication(feed)?;
    Ok(SyndFeed {
        title: feed.child("title", ATOM_10).map(atom_10_text),
        description: feed.child("subtitle", ATOM_10).map(atom_10_text),
        wire: Wire::Atom10,
    })
}

/// `Atom10Parser.parseEntry` and `ConverterForAtom10.createSyndEntry`.
fn atom_10_entry(entry: &Element) -> SyndEntry {
    let link = entry
        .children_named("link", ATOM_10)
        .find(|link| {
            feed_attribute(link, "rel", ATOM_10)
                .is_none_or(|rel| java::trim(rel).is_empty() || rel == "alternate")
        })
        .and_then(|link| feed_attribute(link, "href", ATOM_10))
        .map(str::to_owned);
    SyndEntry {
        link,
        title: entry.child("title", ATOM_10).map(atom_10_text),
        description: entry.child("summary", ATOM_10).map(atom_10_text),
    }
}

/// `Atom03Parser.parseContent`'s value: by the content's mode, its text, its text decoded from
/// Base64 or its markup written out. `Content.setMode` throws on any other mode.
fn atom_03_content(element: &Element) -> Result<String, Thrown> {
    match feed_attribute(element, "mode", ATOM_03).unwrap_or("xml") {
        "escaped" => Ok(element.text()),
        "base64" => rome_base64(&element.text()),
        "xml" => {
            let mut value = String::new();
            jdom::output(&element.content, &mut value);
            Ok(value)
        }
        mode => Err(format!("IllegalArgumentException: Invalid mode [{mode}]")),
    }
}

/// `Atom03Parser.parseFeed` and `ConverterForAtom03.copyInto`.
fn atom_03(feed: &Element) -> Result<SyndFeed, Thrown> {
    let title = feed
        .child("title", ATOM_03)
        .map(atom_03_content)
        .transpose()?;
    let description = feed
        .child("tagline", ATOM_03)
        .map(atom_03_content)
        .transpose()?;
    if let Some(info) = feed.child("info", ATOM_03) {
        atom_03_content(info)?;
    }
    check_syndication(feed)?;
    Ok(SyndFeed {
        title,
        description,
        wire: Wire::Atom03,
    })
}

/// `Atom03Parser.parseEntry` and `ConverterForAtom03.createSyndEntry`, or what they throw.
fn atom_03_entry(entry: &Element) -> Result<SyndEntry, Thrown> {
    let title = entry
        .child("title", ATOM_03)
        .map(atom_03_content)
        .transpose()?;
    let description = entry
        .child("summary", ATOM_03)
        .map(atom_03_content)
        .transpose()?;
    for content in entry.children_named("content", ATOM_03) {
        atom_03_content(content)?;
    }
    let mut alternates = entry
        .children_named("link", ATOM_03)
        .filter(|link| feed_attribute(link, "rel", ATOM_03) == Some("alternate"));
    let link = match (alternates.next(), alternates.next()) {
        (Some(only), None) => feed_attribute(only, "href", ATOM_03).map(str::to_owned),
        _ => None,
    };
    Ok(SyndEntry {
        link,
        title,
        description,
    })
}

/// `Base64.decode` of ROME: line breaks dropped, the rest encoded with the default charset,
/// UTF-8, and decoded with ROME's own decoder, which throws on any byte beyond ASCII and on a
/// length that is not a multiple of four, and reads past the characters it keeps when the text
/// holds others; the bytes decoded as UTF-8.
fn rome_base64(text: &str) -> Result<String, Thrown> {
    const ALPHABET: &[u8; 65] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    let out_of_bounds = || "ArrayIndexOutOfBoundsException".to_owned();
    let code = |byte: u8| -> Result<u8, Thrown> {
        if !byte.is_ascii() {
            return Err(out_of_bounds());
        }
        Ok(ALPHABET
            .iter()
            .position(|&letter| letter == byte)
            .map_or(64, |index| index as u8))
    };
    let encoded: Vec<u8> = text
        .bytes()
        .filter(|&byte| byte != b'\n' && byte != b'\r')
        .collect();
    let mut clean = encoded.clone();
    let mut clean_length = 0;
    for &byte in &encoded {
        if code(byte)? < 64 {
            clean[clean_length] = byte;
            clean_length += 1;
        }
    }
    let decoded_length = clean_length / 4 * 3
        + match clean_length % 4 {
            3 => 2,
            2 => 1,
            _ => 0,
        };
    let mut decoded = vec![0u8; decoded_length];
    let mut index = 0;
    for start in (0..encoded.len()).step_by(4) {
        if start + 3 > encoded.len() {
            return Err("IllegalArgumentException: byte array is not a valid \
                 com.rometools.rome.io.impl.Base64 encoding"
                .to_owned());
        }
        let quad = clean.get(start..start + 4).ok_or_else(out_of_bounds)?;
        let [e1, e2, e3, e4] = [
            u32::from(code(quad[0])?),
            u32::from(code(quad[1])?),
            u32::from(code(quad[2])?),
            u32::from(code(quad[3])?),
        ];
        let bytes = [e1 << 2 | e2 >> 4, e2 << 4 | e3 >> 2, e3 << 6 | e4].map(|byte| byte as u8);
        *decoded.get_mut(index).ok_or_else(out_of_bounds)? = bytes[0];
        index += 1;
        for &byte in &bytes[1..] {
            if index < decoded.len() {
                decoded[index] = byte;
                index += 1;
            }
        }
    }
    Ok(java::utf8(&decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A feed's title, description and entries, as ROME reads them.
    struct Read {
        title: Option<String>,
        description: Option<String>,
        entries: Vec<SyndEntry>,
    }

    fn feed(xml: &str) -> Result<Read, Thrown> {
        let feed = read(xml.as_bytes())?;
        let mut entries = Vec::new();
        let read = each_entry(xml.as_bytes(), &feed, &mut |entry| {
            entries.push(entry);
            Ok::<_, ()>(())
        });
        assert!(read.is_ok());
        Ok(Read {
            title: feed.title,
            description: feed.description,
            entries,
        })
    }

    fn entry(link: &str, title: &str, description: Option<&str>) -> SyndEntry {
        SyndEntry {
            link: Some(link.to_owned()),
            title: Some(title.to_owned()),
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn reads_rss_as_rome_does() {
        let read = feed(
            "<rss version='2.0'><channel><title>T</title><description>D</description>\
             <item><title>A</title><link>l</link><description>x <b>y</b> &amp; z</description></item>\
             <item><title>B</title><guid>g</guid></item>\
             <item><title>C</title><guid isPermaLink='false'>g</guid></item></channel></rss>",
        )
        .unwrap();
        assert_eq!(read.title.as_deref(), Some("T"));
        assert_eq!(read.description.as_deref(), Some("D"));
        assert_eq!(
            read.entries,
            [
                entry("l", "A", Some("x <b>y</b> & z")),
                entry("g", "B", None),
                SyndEntry {
                    link: None,
                    title: Some("C".into()),
                    description: None
                },
            ]
        );
        let userland = feed(
            "<rss version='0.91'><channel><item><link>l</link><description>a<b>b</b></description></item></channel></rss>",
        )
        .unwrap();
        assert_eq!(userland.entries[0].description.as_deref(), Some("a"));
        let rdf = feed(
            "<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns='http://purl.org/rss/1.0/'>\
             <channel><title>T</title></channel><item><link>l</link><title>A</title><description>d</description></item></rdf:RDF>",
        )
        .unwrap();
        assert_eq!(rdf.entries, [entry("l", "A", Some("d"))]);
    }

    #[test]
    fn reads_atom_as_rome_does() {
        let read = feed(
            "<feed xmlns='http://www.w3.org/2005/Atom'><title type='xhtml'><div>T &lt;</div></title>\
             <entry><title>A</title><link rel='self' href='s'/><link href='l'/><summary>S</summary></entry>\
             <entry><link rel='alternate'/></entry></feed>",
        )
        .unwrap();
        assert_eq!(read.title.as_deref(), Some("<div>T &lt;</div>"));
        assert_eq!(read.description, None);
        assert_eq!(
            read.entries,
            [entry("l", "A", Some("S")), SyndEntry::default()]
        );
        let old = feed(
            "<feed xmlns='http://purl.org/atom/ns#'><title mode='escaped'>T</title>\
             <entry><link rel='alternate' href='l'/><title mode='base64'>QQ==</title><summary>S</summary></entry></feed>",
        )
        .unwrap();
        assert_eq!(old.title.as_deref(), Some("T"));
        assert_eq!(old.entries, [entry("l", "A", Some("S"))]);
    }

    #[test]
    fn throws_where_rome_throws() {
        for xml in [
            "<!DOCTYPE rss><rss/>",
            "<rss version='1.0'><channel/></rss>",
            "<rss version='2.0'/>",
            "<rss><channel><skipHours><hour>x</hour></skipHours></channel></rss>",
            "<rss version='0.92'><channel><cloud port=' 8o '/></channel></rss>",
            "<feed xmlns='http://www.w3.org/2005/Atom'><link rel='self'/></feed>",
            "<feed xmlns='http://www.w3.org/2005/Atom' xmlns:sy='http://purl.org/rss/1.0/modules/syndication/'><sy:updatePeriod>often</sy:updatePeriod></feed>",
            "<feed xmlns='http://purl.org/atom/ns#'><info mode='base64'>QQ=</info></feed>",
            "<feed xmlns='http://purl.org/atom/ns#'><entry><content mode='XML'/></entry></feed>",
        ] {
            assert!(feed(xml).is_err(), "{xml}");
        }
        assert!(feed("<rss version='0.91'><channel><cloud port='x'/></channel></rss>").is_ok());
    }

    #[test]
    fn decodes_base64_as_rome_does() {
        assert_eq!(rome_base64("SGVs\nbG8=").as_deref(), Ok("Hello"));
        assert_eq!(rome_base64("").as_deref(), Ok(""));
        assert!(rome_base64("QUJD*").is_err());
        assert!(rome_base64("QUJ\u{e9}").is_err());
        assert!(rome_base64("QQ").is_err());
    }
}
