//! RSS and Atom feeds, as Tika's `FeedParser` writes what ROME reads of them ([`rome`]): the
//! feed's title and description as `dc:title` and `dc:description` and as a heading and a
//! paragraph, then a list of the entries that have a link, each its title in an anchor to the
//! link and its description after a line break, all with their tags stripped.

mod jdom;
mod rome;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};
use crate::java;

pub struct FeedParser;

impl Parser for FeedParser {
    fn name(&self) -> &'static str {
        "extractous::FeedParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.feed.FeedParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/rss+xml", "application/atom+xml"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let feed = rome::read(input).map_err(|thrown| {
            Declined::Fails(Error::ParseError(format!("RSS parse error: {thrown}")))
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
            for entry in &feed.entries {
                let Some(link) = &entry.link else {
                    continue;
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
            }
            xhtml.end_element("ul")?;
            Ok(())
        }))
    }
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
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.feed.FeedParser";

    #[test]
    fn strips_tags_as_tika_does() {
        assert_eq!(strip_tags(None), "");
        assert_eq!(strip_tags(Some(" a<b>c</b> <x y='>'d ")), "ac 'd");
        assert_eq!(strip_tags(Some("a < b")), "a < b");
        assert_eq!(strip_tags(Some("<![CDATA[x]]>y<!--a>b-->")), "yb-->");
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_feeds_as_tikas_path_does() {
        let scratch = Scratch::new("feed");
        let read = [
            scratch.write(
                "rss2.rss",
                b"<?xml version='1.0'?><rss version='2.0' xmlns:sy='http://purl.org/rss/1.0/modules/syndication/'>\
                  <channel><title> Nyheder &amp; <b>mere</b> </title><description>Om <![CDATA[<i>alt</i>]]>\
                  </description><item><title>F\xc3\xb8rste</title><link>http://a/1</link><description>\
                  Tekst <p>med <!-- c --> &lt;markup&gt;</p> og \xf0\x9f\x98\x80\n<br/>linje</description></item>\
                  <item><title>Uden link</title><description>skjult</description></item>\
                  <item><title>Guid</title><guid>http://a/g</guid></item></channel></rss>",
            ),
            scratch.write(
                "rss091.rss",
                b"<rss version='0.91'><channel><title>T</title><item><title>A</title><link>l</link>\
                  <description>a<b>b</b>c</description></item></channel></rss>",
            ),
            scratch.write(
                "rdf.rss",
                b"<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns='http://purl.org/rss/1.0/'>\
                  <channel><title>T</title><description>D</description></channel>\
                  <item><title>A</title><link>l</link><description>d</description></item></rdf:RDF>",
            ),
            scratch.write(
                "atom.atom",
                b"<feed xmlns='http://www.w3.org/2005/Atom'><title type='xhtml'><div xmlns='http://www.w3.org/1999/xhtml'>\
                  T &lt;x&gt;</div></title><subtitle type='html'>&lt;b&gt;S&lt;/b&gt;</subtitle>\
                  <entry><title>A</title><link rel='self' href='s'/><link href='l'/><summary>S</summary></entry>\
                  <entry><title>B</title><link rel='alternate'/></entry></feed>",
            ),
            scratch.write(
                "atom03.atom",
                b"<feed xmlns='http://purl.org/atom/ns#'><title mode='escaped'>T</title><tagline>tag<b>line</b></tagline>\
                  <entry><link rel='alternate' href='l'/><title mode='base64'>QQ==</title>\
                  <summary mode='escaped'>&lt;b&gt;x</summary></entry></feed>",
            ),
        ];
        let failing = [
            scratch.write("doctype.rss", b"<!DOCTYPE rss><rss version='2.0'><channel/></rss>"),
            scratch.write("no-channel.rss", b"<rss version='2.0'/>"),
            scratch.write("version.rss", b"<rss version='1.0'><channel/></rss>"),
            scratch.write(
                "hours.rss",
                b"<rss><channel><skipHours><hour>x</hour></skipHours></channel></rss>",
            ),
            scratch.write("malformed.rss", b"<rss version='2.0'><channel></rss>"),
            scratch.write(
                "mode.atom",
                b"<feed xmlns='http://purl.org/atom/ns#'><entry><summary mode='unknown'/></entry></feed>",
            ),
        ];
        let (parsed, deferred) = documents(&FeedParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_feeds_as_tikas_path_does() {
        let (parsed, deferred) = documents(&FeedParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
