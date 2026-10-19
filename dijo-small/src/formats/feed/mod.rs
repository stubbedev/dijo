//! RSS and Atom feeds, as Tika's `FeedParser` writes what ROME reads of them ([`rome`]): the
//! feed's title and description as `dc:title` and `dc:description` and as a heading and a
//! paragraph, then a list of the entries that have a link, each its title in an anchor to the
//! link and its description after a line break, all with their tags stripped. The entries are
//! read and written one at a time.

mod jdom;
mod rome;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java;

pub struct FeedParser;

impl Parser for FeedParser {
    fn name(&self) -> &'static str {
        "dijo::FeedParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.feed.FeedParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/rss+xml", "application/atom+xml"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let feed = rome::read(input).map_err(|thrown| {
            Decline::Fails(Error::ParseError(format!("RSS parse error: {thrown}")))
        })?;
        Ok(Box::new(move |xhtml| {
            let title = strip_tags(feed.title.as_deref());
            let description = strip_tags(feed.description.as_deref());
            let metadata = xhtml.metadata_mut();
            metadata.set("dc:title", title.clone());
            metadata.set("dc:description", description.clone());
            xhtml.element("h1", &title)?;
            xhtml.element("p", &description)?;
            xhtml.start_element("ul")?;
            rome::each_entry(input, &feed, &mut |entry| write_entry(&entry, xhtml)).map_err(
                |halt| match halt {
                    rome::Halt::Taken(failure) => failure,
                    rome::Halt::Reread(reason) => Failure::Malformed(format!(
                        "the feed reads otherwise the second time: {reason}"
                    )),
                },
            )?;
            xhtml.end_element("ul")?;
            Ok(())
        }))
    }
}

/// An entry that has a link, as an item of the list: its title in an anchor to the link, and
/// its description after a line break.
fn write_entry(entry: &rome::SyndEntry, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let Some(link) = &entry.link else {
        return Ok(());
    };
    xhtml.start_element("li")?;
    xhtml.start_element_with("a", &[("href", link)])?;
    xhtml.characters(&strip_tags(entry.title.as_deref()))?;
    xhtml.end_element("a")?;
    if let Some(description) = &entry.description {
        xhtml.newline()?;
        xhtml.characters(&strip_tags(Some(description)))?;
    }
    xhtml.end_element("li")?;
    Ok(())
}

/// `FeedParser.stripTags` of a text construct's value: every match of `<[^>]*>` removed, then
/// trimmed; the empty string for no value.
fn strip_tags(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let mut stripped = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        stripped.push_str(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    stripped.push_str(rest);
    java::trim(&stripped).to_owned()
}

#[cfg(test)]
mod tests {

    use super::*;

    /// A feed too large to read whole, its title after its items: each item is written as it is
    /// read the second time, under a budget far below the feed.
    #[test]
    fn writes_feeds_too_large_to_read_whole_an_entry_at_a_time() {
        use dijo_core::large::{bodies_past_limit, LargeFile};

        let item = |index: u64| {
            format!(
                "<item><title>Nyhed {index}</title><link>http://a/{index}</link>\
                 <description>Tekst</description></item>\n"
            )
            .into_bytes()
        };
        let count = bodies_past_limit(item(0).len());
        let file = LargeFile::new(
            "large.rss",
            b"<?xml version='1.0'?><rss version='2.0'><channel>\n",
            count,
            item,
            b"<title>Nyheder</title></channel></rss>",
        );
        let mut items = 0u64;
        let metadata = file.extract_lines(FeedParser.name(), |line| {
            if line.starts_with("\tNyhed ") {
                assert_eq!(line, format!("\tNyhed {items}"));
                items += 1;
            }
        });
        assert_eq!(items, count);
        assert_eq!(metadata["dc:title"], ["Nyheder"]);
    }

    #[test]
    fn strips_tags_as_tika_does() {
        assert_eq!(strip_tags(None), "");
        assert_eq!(strip_tags(Some(" a<b>c</b> <x y='>'d ")), "ac 'd");
        assert_eq!(strip_tags(Some("a < b")), "a < b");
        assert_eq!(strip_tags(Some("<![CDATA[x]]>y<!--a>b-->")), "yb-->");
    }
}
