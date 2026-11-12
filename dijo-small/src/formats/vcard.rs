//! vCard files (RFC 6350 4.0, RFC 2426 3.0 and vCard 2.1), read through an [`AutoDetectReader`]
//! as Tika reads them as text, their content lines unfolded, their values unescaped and their
//! quoted-printable values decoded in the charset their `CHARSET` parameter names. Each card is a
//! section, `<div class="vcard">` as hCard names it: its formatted name (`FN`), else its
//! structured name (`N`) assembled, as a heading, then its nicknames, organisation, titles, roles,
//! birthday and anniversary, its email addresses written as mail writes a mailbox, its phone
//! numbers and addresses labelled with their types, its websites and categories, its notes a
//! paragraph a line, and its agents, a 2.1 card nested in it or a 3.0 card in its `AGENT` text,
//! as sections of their own within it. Its photos, logos and sounds held inline, in base64 or a
//! `data:` URI, are handed to the embedded document extractor, named by the card and the type
//! they declare or are detected as; those held elsewhere are listed by their URI and never
//! fetched. The formatted name of a file of one card is its `dc:title`; a file of several cards
//! has none, as no one of them names it.
//!
//! The cards are written as they end, line by line, so that only the card being read is held.

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::content_line::{unfold, ContentLine, Encoding, Person};
use crate::formats::data_uri;
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::mime::{self, MediaType, Origin};

/// How deep cards may nest as agents of cards.
const MAX_DEPTH: usize = 8;

/// The types that say how a value is used rather than what it is, which no label shows.
const UNLABELLED_TYPES: [&str; 2] = ["PREF", "INTERNET"];

pub struct VCardParser;

impl Parser for VCardParser {
    fn name(&self) -> &'static str {
        "dijo::VCardParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::VCardParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["text/vcard", "text/x-vcard"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        unless_different(
            Box::new(move |xhtml| write(input, pages, xhtml)),
            AutoDetectReader::difference(input, metadata.get("Content-Type")),
        )
    }
}

/// Writes the cards of `input`, decoded as Tika's `AutoDetectReader` decodes it, the `pages` of
/// `input` released as they are read.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let content_type = xhtml
        .metadata_mut()
        .get("Content-Type")
        .ok_or_else(|| Failure::Malformed("the document has no Content-Type".into()))?
        .to_owned();
    let (detection, text) = AutoDetectReader::stream(input, Some(&content_type), pages);
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&detection, metadata);
    metadata.set("Content-Encoding", detection.decode_as.name());
    let mut cards = 0usize;
    let mut title = None;
    walk(text.lines(), 0, &mut |card| {
        cards += 1;
        if cards == 1 {
            title = card.formatted_name.clone();
        }
        card.write(xhtml)
    })?;
    if let (1, Some(title)) = (cards, title) {
        xhtml.metadata_mut().set("dc:title", title);
    }
    Ok(())
}

/// A card begun and not yet ended.
struct Open {
    line: usize,
    card: Card,
}

/// Reads the content lines of `lines`, the physical lines of cards within `depth` cards, handing
/// each card to `ended` as it ends.
fn walk(
    lines: impl Iterator<Item = String>,
    depth: usize,
    ended: &mut dyn FnMut(Card) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut open: Vec<Open> = Vec::new();
    for logical in unfold(lines) {
        let logical = logical?;
        let at =
            |message: String| Failure::Malformed(format!("line {}: {message}", logical.number));
        let line = ContentLine::parse(&logical.text).map_err(at)?;
        if line.is("BEGIN") {
            if !line.raw.eq_ignore_ascii_case("VCARD") {
                return Err(at(format!("BEGIN:{} begins no vCard", line.raw)));
            }
            if depth + open.len() == MAX_DEPTH {
                return Err(at(format!("vCards nest deeper than {MAX_DEPTH}")));
            }
            open.push(Open {
                line: logical.number,
                card: Card::default(),
            });
        } else if line.is("END") {
            if !line.raw.eq_ignore_ascii_case("VCARD") {
                return Err(at(format!("END:{} ends no vCard", line.raw)));
            }
            let card = open
                .pop()
                .ok_or_else(|| at("END:VCARD ends no vCard".to_owned()))?
                .card;
            match open.last_mut() {
                Some(parent) => parent.card.agents.push(Agent::Card(Box::new(card))),
                None => ended(card)?,
            }
        } else {
            let held = depth + open.len();
            let Some(current) = open.last_mut() else {
                return Err(at(format!("the {} line is outside any vCard", line.name)));
            };
            current
                .card
                .property(&line, held)
                .map_err(|failure| match failure {
                    Failure::Malformed(message) => at(message),
                    other => other,
                })?;
        }
    }
    match open.last() {
        Some(unended) => Err(Failure::Malformed(format!(
            "the file ends inside the vCard begun at line {}",
            unended.line
        ))),
        None => Ok(()),
    }
}

/// A value with the types its `TYPE` parameters give it, such as `work` or `cell`.
struct Typed {
    types: Vec<String>,
    value: String,
}

impl Typed {
    /// The value `value` of `line`, typed by its `TYPE` parameters, lowercased, each list of
    /// them split, those of [`UNLABELLED_TYPES`] left out.
    fn of(line: &ContentLine, value: String) -> Self {
        let types = line
            .parameter_values("TYPE")
            .flat_map(|types| types.split(','))
            .map(str::trim)
            .filter(|kind| {
                !kind.is_empty()
                    && !UNLABELLED_TYPES
                        .iter()
                        .any(|unlabelled| kind.eq_ignore_ascii_case(unlabelled))
            })
            .map(str::to_ascii_lowercase)
            .collect();
        Self { types, value }
    }

    /// `label`, followed by the types in parentheses if there are any.
    fn label(&self, label: &str) -> String {
        match self.types.is_empty() {
            true => label.to_owned(),
            false => format!("{label} ({})", self.types.join(", ")),
        }
    }
}

/// What a photo, a logo or a sound is to its card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    Photo,
    Logo,
    Sound,
}

impl MediaKind {
    fn named(name: &str) -> Option<Self> {
        [Self::Photo, Self::Logo, Self::Sound]
            .into_iter()
            .find(|kind| name.eq_ignore_ascii_case(kind.property()))
    }

    fn property(self) -> &'static str {
        match self {
            Self::Photo => "PHOTO",
            Self::Logo => "LOGO",
            Self::Sound => "SOUND",
        }
    }

    /// The word an embedded one is named with.
    fn word(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Logo => "logo",
            Self::Sound => "sound",
        }
    }

    /// The label a reference to it is listed with.
    fn label(self) -> &'static str {
        match self {
            Self::Photo => "Photo",
            Self::Logo => "Logo",
            Self::Sound => "Sound",
        }
    }

    /// The top-level type of a vCard 2.1 or 3.0 `TYPE` that names its format alone, such as
    /// `JPEG`.
    fn top_level(self) -> &'static str {
        match self {
            Self::Photo | Self::Logo => "image",
            Self::Sound => "audio",
        }
    }
}

/// Where a photo, a logo or a sound is.
enum Source {
    /// In the card, its bytes decoded, of the type it declares or is detected as.
    Inline {
        data: Vec<u8>,
        media_type: MediaType,
    },
    /// At a URI, which is never fetched.
    Reference(String),
}

/// A photo, a logo or a sound of a card.
struct Media {
    kind: MediaKind,
    source: Source,
}

impl Media {
    /// The medium of the `kind` line `line`: base64 bytes, a `data:` URI, or a reference.
    fn of(kind: MediaKind, line: &ContentLine) -> Result<Self, Failure> {
        let malformed = |message: String| Failure::Malformed(message);
        let source = if line.encoding().map_err(malformed)? == Encoding::Base64 {
            let data = line.binary().map_err(malformed)?;
            let media_type = match declared_type(kind, line) {
                Some(media_type) => media_type,
                None => detected_type(kind, &data)?,
            };
            Source::Inline { data, media_type }
        } else {
            let uri = line.raw.trim();
            match uri.get(..5) {
                Some(scheme) if scheme.eq_ignore_ascii_case("data:") => data_uri_source(kind, uri)?,
                _ => Source::Reference(line.text().map_err(malformed)?.trim().to_owned()),
            }
        };
        Ok(Self { kind, source })
    }

    /// Lists a referenced medium, or hands an inline one to the embedded document extractor,
    /// named for the card named `holder`, if it is named.
    fn write(&self, holder: Option<&str>, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let (data, media_type) = match &self.source {
            Source::Reference(uri) => {
                let label = self.kind.label();
                return Ok(xhtml.element("p", &format!("{label}: {uri}"))?);
            }
            Source::Inline { data, media_type } => (data, media_type),
        };
        let name = match holder {
            Some(holder) => format!("{holder} {}", self.kind.word()),
            None => self.kind.word().to_owned(),
        };
        let mut metadata = TikaMetadata::new();
        metadata.set(
            RESOURCE_NAME,
            format!("{name}{}", mime::extension(media_type)),
        );
        metadata.set("Content-Type", media_type.to_string());
        ResourceType::Inline.set(&mut metadata);
        xhtml.parse_embedded(data, &metadata, Wrapping::Bare)
    }
}

/// The type the `MEDIATYPE` or `TYPE` parameters of the `kind` line `line` declare, if the
/// default types declare it: a media type, or a vCard 2.1 or 3.0 format name such as `JPEG`.
fn declared_type(kind: MediaKind, line: &ContentLine) -> Option<MediaType> {
    line.parameter("MEDIATYPE")
        .into_iter()
        .chain(line.parameter_values("TYPE"))
        .find_map(|declared| {
            let name = match declared.contains('/') {
                true => declared.to_ascii_lowercase(),
                false => format!("{}/{}", kind.top_level(), declared.to_ascii_lowercase()),
            };
            MediaType::parse(&name).filter(mime::declares)
        })
}

/// The type Tika's detection gives the bytes `data` of a medium of `kind`.
fn detected_type(kind: MediaKind, data: &[u8]) -> Result<MediaType, Failure> {
    mime::detect(data, Origin::Bytes)
        .map(|detection| detection.media_type)
        .map_err(|error| {
            Failure::Malformed(format!(
                "detecting the type of the {} fails: {error}",
                kind.property()
            ))
        })
}

/// The medium of `kind` the `data:` URI `uri` holds.
fn data_uri_source(kind: MediaKind, uri: &str) -> Result<Source, Failure> {
    let in_uri =
        |message: &str| Failure::Malformed(format!("the {} data URI {message}", kind.property()));
    let parsed = data_uri::parse(uri).ok_or_else(|| in_uri("is malformed"))?;
    let data = parsed.bytes().map_err(|thrown| {
        in_uri(&format!(
            "does not decode: {}",
            thrown.into_error(VCardParser.tika_name())
        ))
    })?;
    let media_type = match parsed.content_type().as_deref().and_then(MediaType::parse) {
        Some(media_type) => media_type,
        None => detected_type(kind, &data)?,
    };
    Ok(Source::Inline { data, media_type })
}

/// What an `AGENT` names: a card, nested or in its text, or one at a URI.
enum Agent {
    Card(Box<Card>),
    Reference(String),
}

/// What is written of a card.
#[derive(Default)]
struct Card {
    formatted_name: Option<String>,
    /// The structured name, assembled.
    name: Option<String>,
    nicknames: Vec<String>,
    organisations: Vec<String>,
    titles: Vec<String>,
    roles: Vec<String>,
    birthday: Option<String>,
    anniversary: Option<String>,
    emails: Vec<Typed>,
    phones: Vec<Typed>,
    addresses: Vec<Typed>,
    urls: Vec<String>,
    categories: Vec<String>,
    notes: Vec<String>,
    agents: Vec<Agent>,
    media: Vec<Media>,
}

impl Card {
    /// Takes in a property of the card, which `depth` cards hold.
    fn property(&mut self, line: &ContentLine, depth: usize) -> Result<(), Failure> {
        let malformed = |message: String| Failure::Malformed(message);
        let text = || {
            line.text()
                .map(|text| text.trim().to_owned())
                .map_err(malformed)
        };
        let texts = |separator| line.texts(separator).map_err(malformed);
        if line.is("FN") {
            self.formatted_name = Some(text()?).filter(|name| !name.is_empty());
        } else if line.is("N") {
            self.name = assembled_name(texts(';')?);
        } else if line.is("NICKNAME") {
            self.nicknames.extend(non_empty(texts(',')?));
        } else if line.is("ORG") {
            let units: Vec<String> = non_empty(texts(';')?).collect();
            if !units.is_empty() {
                self.organisations.push(units.join(", "));
            }
        } else if line.is("TITLE") {
            self.titles.push(text()?);
        } else if line.is("ROLE") {
            self.roles.push(text()?);
        } else if line.is("BDAY") {
            self.birthday = Some(date(line, text()?));
        } else if line.is("ANNIVERSARY") {
            self.anniversary = Some(date(line, text()?));
        } else if line.is("EMAIL") {
            self.emails.push(Typed::of(line, text()?));
        } else if line.is("TEL") {
            let number = text()?;
            let number = match number.get(..4) {
                Some(scheme) if scheme.eq_ignore_ascii_case("tel:") => number[4..].to_owned(),
                _ => number,
            };
            self.phones.push(Typed::of(line, number));
        } else if line.is("ADR") {
            if let Some(address) = assembled_address(texts(';')?) {
                self.addresses.push(Typed::of(line, address));
            }
        } else if line.is("URL") {
            self.urls.push(text()?);
        } else if line.is("CATEGORIES") {
            self.categories.extend(non_empty(texts(',')?));
        } else if line.is("NOTE") {
            self.notes.push(text()?);
        } else if line.is("AGENT") {
            self.agent(line, text()?, depth)?;
        } else if let Some(kind) = MediaKind::named(line.name) {
            self.media.push(Media::of(kind, line)?);
        }
        Ok(())
    }

    /// Takes in the `AGENT` line `line` of the text `text`: a 3.0 card in its text, a URI, or,
    /// empty, the 2.1 card nested after it.
    fn agent(&mut self, line: &ContentLine, text: String, depth: usize) -> Result<(), Failure> {
        if text.is_empty() {
            return Ok(());
        }
        let is_uri = line
            .parameter("VALUE")
            .is_some_and(|value| value.eq_ignore_ascii_case("uri"));
        if is_uri
            || !text
                .get(..6)
                .is_some_and(|begin| begin.eq_ignore_ascii_case("BEGIN:"))
        {
            self.agents.push(Agent::Reference(text));
            return Ok(());
        }
        walk(text.lines().map(str::to_owned), depth, &mut |card| {
            self.agents.push(Agent::Card(Box::new(card)));
            Ok(())
        })
        .map_err(|failure| match failure {
            Failure::Malformed(message) => {
                Failure::Malformed(format!("the AGENT's vCard: {message}"))
            }
            other => other,
        })
    }

    /// The name it is written under: its formatted name, else its structured name assembled.
    fn display_name(&self) -> Option<&str> {
        self.formatted_name.as_deref().or(self.name.as_deref())
    }

    /// Writes the card as a section.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml.start_element_with("div", &[("class", "vcard")])?;
        let holder = self.display_name();
        if let Some(holder) = holder {
            xhtml.element("h2", holder)?;
        }
        let nicknames = self.nicknames.join(", ");
        let categories = self.categories.join(", ");
        let listed = [
            ("Nickname", (!nicknames.is_empty()).then_some(&nicknames)),
            ("Born", self.birthday.as_ref()),
            ("Anniversary", self.anniversary.as_ref()),
        ];
        let fields = self
            .organisations
            .iter()
            .map(|organisation| ("Organisation", organisation))
            .chain(self.titles.iter().map(|title| ("Title", title)))
            .chain(self.roles.iter().map(|role| ("Role", role)))
            .chain(
                listed
                    .into_iter()
                    .filter_map(|(label, value)| Some((label, value?))),
            );
        for (label, value) in fields {
            xhtml.element("p", &format!("{label}: {value}"))?;
        }
        for email in &self.emails {
            let mailbox = Person::new(holder.map(str::to_owned), &email.value);
            xhtml.element("p", &format!("{}: {mailbox}", email.label("Email")))?;
        }
        for (label, typed) in self
            .phones
            .iter()
            .map(|phone| ("Phone", phone))
            .chain(self.addresses.iter().map(|address| ("Address", address)))
        {
            xhtml.element("p", &format!("{}: {}", typed.label(label), typed.value))?;
        }
        for url in &self.urls {
            xhtml.element("p", &format!("Website: {url}"))?;
        }
        if !categories.is_empty() {
            xhtml.element("p", &format!("Categories: {categories}"))?;
        }
        for note in &self.notes {
            for paragraph in note.lines() {
                xhtml.element("p", paragraph.trim())?;
            }
        }
        for agent in &self.agents {
            match agent {
                Agent::Card(card) => {
                    xhtml.element("p", "Agent:")?;
                    card.write(xhtml)?;
                }
                Agent::Reference(uri) => xhtml.element("p", &format!("Agent: {uri}"))?,
            }
        }
        for media in &self.media {
            media.write(holder, xhtml)?;
        }
        Ok(xhtml.end_element("div")?)
    }
}

/// The non-empty values of `values`.
fn non_empty(values: Vec<String>) -> impl Iterator<Item = String> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The structured name of the components `family;given;additional;prefixes;suffixes`, in the
/// order it is spoken: prefixes, given, additional and family names, then suffixes.
fn assembled_name(components: Vec<String>) -> Option<String> {
    let component = |at: usize| components.get(at).map(String::as_str).unwrap_or_default();
    let parts: Vec<&str> = [3, 1, 2, 0, 4]
        .into_iter()
        .map(|at| component(at).trim())
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The address of the components `post office box;extended;street;locality;region;postal
/// code;country`, on one line: the box, the extended and street addresses, the postal code and
/// locality, the region and the country, separated by commas.
fn assembled_address(components: Vec<String>) -> Option<String> {
    let component = |at: usize| {
        components
            .get(at)
            .map(|part| part.trim())
            .unwrap_or_default()
    };
    let town = [component(5), component(3)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let parts: Vec<&str> = [
        component(0),
        component(1),
        component(2),
        &town,
        component(4),
        component(6),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The date `value` of `line`: as written if it is text, else an ISO 8601 basic date, with or
/// without a year, in its extended form, and any other date or time as written.
fn date(line: &ContentLine, value: String) -> String {
    let is_text = line
        .parameter("VALUE")
        .is_some_and(|kind| kind.eq_ignore_ascii_case("text"));
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    let bytes = value.as_bytes();
    if is_text {
        value
    } else if bytes.len() >= 8 && digits(&value[..8]) && bytes.get(8).is_none_or(|&b| b == b'T') {
        format!(
            "{}-{}-{}{}",
            &value[..4],
            &value[4..6],
            &value[6..8],
            &value[8..]
        )
    } else if bytes.len() == 6 && value.starts_with("--") && digits(&value[2..]) {
        format!("--{}-{}", &value[2..4], &value[4..])
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text and metadata the native path extracts from `cards`.
    fn read(cards: &[u8]) -> crate::ExtractResult<(String, crate::Metadata)> {
        crate::Extractor::new().extract_bytes_to_string(cards)
    }

    /// Why reading `cards` fails.
    fn failure(cards: &str) -> String {
        match walk(cards.lines().map(str::to_owned), 0, &mut |_| Ok(())) {
            Err(Failure::Malformed(message)) => message,
            Err(other) => panic!("{other:?}"),
            Ok(()) => panic!("{cards:?} is read"),
        }
    }

    #[test]
    fn writes_each_card_as_a_section() {
        let cards = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Lund;Ada;Marie;Dr.;\r\n\
            ORG:Skagen Fyr;Lamp\\, lens and oil\r\nTITLE:Keeper\r\n\
            EMAIL;TYPE=INTERNET,WORK:ada@example.org\r\nTEL;TYPE=CELL;TYPE=PREF:+45 1234\r\n\
            ADR;TYPE=WORK:;Tower;Fyrvej 36;Skagen;;9990;Denmark\r\nBDAY:19700415\r\n\
            NOTE:Lights the lamp at dusk.\\nLogs it.\r\nCATEGORIES:Keepers,Skagen\r\n\
            URL:https://example.org/ada\r\nPHOTO;VALUE=uri:https://example.org/ada.jpg\r\n\
            END:VCARD\r\nBEGIN:VCARD\r\nVERSION:4.0\r\nFN:Bo\r\nTEL;VALUE=uri:tel:+45-5678\r\n\
            ANNIVERSARY:--0624\r\nEND:VCARD\r\n";
        let (text, metadata) = read(cards.as_bytes()).unwrap();
        assert_eq!(
            text.trim(),
            "Dr. Ada Marie Lund\n\n\
             Organisation: Skagen Fyr, Lamp, lens and oil\n\n\
             Title: Keeper\n\n\
             Born: 1970-04-15\n\n\
             Email (work): Dr. Ada Marie Lund <ada@example.org>\n\n\
             Phone (cell): +45 1234\n\n\
             Address (work): Tower, Fyrvej 36, 9990 Skagen, Denmark\n\n\
             Website: https://example.org/ada\n\n\
             Categories: Keepers, Skagen\n\n\
             Lights the lamp at dusk.\n\n\
             Logs it.\n\n\
             Photo: https://example.org/ada.jpg\n\n\n\
             Bo\n\n\
             Anniversary: --06-24\n\n\
             Phone: +45-5678"
        );
        assert_eq!(metadata["Content-Type"], ["text/x-vcard"]);
        assert!(!metadata.contains_key("dc:title"));
    }

    #[test]
    fn decodes_quoted_printable_values_and_reads_nested_agents() {
        let cards =
            b"BEGIN:VCARD\r\nVERSION:2.1\r\nFN;CHARSET=ISO-8859-1;QUOTED-PRINTABLE:Ove Gr=F8n\r\n\
            NOTE;ENCODING=QUOTED-PRINTABLE:First line=0D=0A=\r\nsecond line\r\n\
            AGENT:\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nFN:Assistant\r\nEND:VCARD\r\n\
            END:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\n\
            AGENT:BEGIN:VCARD\\nFN:Deputy\\nEMAIL\\;INTERNET:deputy@example.org\\nEND:VCARD\\n\r\n\
            END:VCARD\r\n";
        let (text, _) = read(cards).unwrap();
        assert_eq!(
            text.trim(),
            "Ove Gr\u{f8}n\n\nFirst line\n\nsecond line\n\nAgent:\n\nAssistant\n\n\n\n\
             Ada\n\nAgent:\n\nDeputy\n\nEmail: Deputy <deputy@example.org>"
        );
    }

    #[test]
    fn titles_a_file_of_one_card_by_its_formatted_name() {
        let (_, metadata) =
            read(b"BEGIN:VCARD\nVERSION:4.0\nFN:Ada Lund\nN:Lund;Ada;;;\nEND:VCARD\n").unwrap();
        assert_eq!(metadata["dc:title"], ["Ada Lund"]);
    }

    #[test]
    fn decodes_inline_media_of_their_declared_or_detected_type() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGP4DwABAQEAWk1v8QAAAABJRU5ErkJggg==";
        let medium = |text: &str| {
            let line = ContentLine::parse(text).unwrap();
            match Media::of(MediaKind::named(line.name).unwrap(), &line) {
                Ok(Media {
                    source: Source::Inline { data, media_type },
                    ..
                }) => (data.len(), media_type.to_string()),
                Ok(Media {
                    source: Source::Reference(uri),
                    ..
                }) => (0, uri),
                Err(failure) => panic!("{failure:?}"),
            }
        };
        assert_eq!(
            medium(&format!("PHOTO;JPEG;ENCODING=BASE64:{PNG}")),
            (67, "image/jpeg".to_owned())
        );
        assert_eq!(
            medium(&format!("LOGO;ENCODING=b;TYPE=X-UNKNOWN:{PNG}")),
            (67, "image/png".to_owned())
        );
        assert_eq!(
            medium(&format!(
                "PHOTO;MEDIATYPE=image/png:data:image/png;base64,{PNG}"
            )),
            (67, "image/png".to_owned())
        );
        assert_eq!(
            medium("SOUND;VALUE=uri:https://example.org/ada.ogg"),
            (0, "https://example.org/ada.ogg".to_owned())
        );
    }

    #[test]
    fn fails_with_the_line_of_a_malformed_card() {
        assert_eq!(
            failure("BEGIN:VCARD\nFN:x\n"),
            "the file ends inside the vCard begun at line 1"
        );
        assert_eq!(failure("END:VCARD\n"), "line 1: END:VCARD ends no vCard");
        assert_eq!(
            failure("FN:x\n"),
            "line 1: the FN line is outside any vCard"
        );
        assert_eq!(
            failure("BEGIN:VCALENDAR\n"),
            "line 1: BEGIN:VCALENDAR begins no vCard"
        );
        assert_eq!(
            failure("BEGIN:VCARD\nPHOTO;ENCODING=b:QQ=\nEND:VCARD\n"),
            "line 2: the PHOTO line's base64 value is malformed"
        );
        assert_eq!(
            failure(&"BEGIN:VCARD\n".repeat(MAX_DEPTH + 1)),
            format!(
                "line {}: vCards nest deeper than {MAX_DEPTH}",
                MAX_DEPTH + 1
            )
        );
        assert_eq!(
            failure("BEGIN:VCARD\nAGENT:BEGIN:VCARD\\nFN:x\nEND:VCARD\n"),
            "line 2: the AGENT's vCard: the file ends inside the vCard begun at line 1"
        );
    }
}
