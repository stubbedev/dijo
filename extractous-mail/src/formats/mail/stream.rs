//! mime4j 0.8.15's `MimeTokenStream`, as `MimeStreamParser` drives it for Tika's `RFC822Parser`:
//! content decoding on, no recursion into attached messages, and Tika's limits of 100,000 bytes
//! a line and a header field. A message is a stack of entities, each reading its lines through
//! a buffer of 4,096 bytes from the part its parent's boundary stream delimits, and the stream
//! reports the parts and fields Tika's handler sees as [`Event`]s. A boundary is recognised at
//! the start of a line alone, wherever the buffers are refilled: mime4j also takes one in the
//! middle of a line where a refilled buffer starts, and one at the end of a buffer whatever
//! follows it (#71).

use super::codec::{decode_base64, decode_quoted_printable};
use super::field::{is_whitespace, RawField};
use super::structured::{ContentDisposition, ContentType};
use super::Unported;

const BUFFER_SIZE: usize = 4096;
const MAX_LINE_LENGTH: usize = 100_000;
const MAX_HEADER_LENGTH: usize = 100_000;
const MAX_HEADER_COUNT: usize = 1000;
const MAX_TOTAL_HEADER_COUNT: usize = 16_384;
const MAX_TOTAL_HEADER_LENGTH: usize = 1024 * 1024;
const MAX_PART_COUNT: usize = 512;
const MAX_NESTING_DEPTH: usize = 64;
/// The bytes `IOUtils.copy` reads at a time.
const COPY_BUFFER_SIZE: usize = 8192;
/// The bytes `MimeEntity` drains a part with at a time: the recycled buffers it gets are at
/// least this large.
const DRAIN_BUFFER_SIZE: usize = 4000;

/// A part of the message as Tika's `MailContentHandler` is told of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Event {
    /// A header field of the message or of one of its parts.
    Field(RawField),
    StartMultipart(BodyDescriptor),
    EndMultipart,
    StartBodyPart,
    EndBodyPart,
    /// A leaf body, its transfer encoding decoded.
    Body(BodyDescriptor, Vec<u8>),
}

/// Why mime4j stops reading a message, which Tika reports as a failure to parse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MimeError(pub(super) String);

/// The `BodyDescriptor` `DefaultBodyDescriptorBuilder` builds for a part from the first of each
/// of its `Content-` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BodyDescriptor {
    pub(super) mime_type: String,
    pub(super) media_type: String,
    pub(super) subtype: String,
    pub(super) boundary: Option<String>,
    pub(super) charset: Option<String>,
    content_type: Option<ContentType>,
    transfer_encoding: Option<String>,
    disposition: Option<String>,
}

impl BodyDescriptor {
    /// `getTransferEncoding`: the field's value trimmed and in lower case, `7bit` without one.
    pub(super) fn transfer_encoding(&self) -> &str {
        self.transfer_encoding.as_deref().unwrap_or("7bit")
    }

    /// A parameter of the `Content-Type` field, as `getContentTypeParameters`.
    pub(super) fn content_type_parameter(&self, name: &str) -> Option<&str> {
        self.content_type
            .as_ref()
            .and_then(|content_type| content_type.parameter(name))
    }

    /// The `Content-Disposition` field, parsed when it is asked for, as mime4j parses it.
    pub(super) fn disposition(&self) -> Result<Option<ContentDisposition>, Unported> {
        self.disposition
            .as_deref()
            .map(ContentDisposition::parse)
            .transpose()
    }
}

/// `DefaultBodyDescriptorBuilder`: the first `Content-Type`, `Content-Transfer-Encoding` and
/// `Content-Disposition` fields of a part, and the type of the part it is in.
#[derive(Debug, Clone, Default)]
struct DescriptorBuilder {
    parent_mime_type: Option<String>,
    content_type: Option<ContentType>,
    transfer_encoding: Option<String>,
    disposition: Option<String>,
}

impl DescriptorBuilder {
    fn reset(&mut self) {
        self.content_type = None;
        self.transfer_encoding = None;
        self.disposition = None;
    }

    fn add_field(&mut self, field: &RawField) {
        match field.name().to_ascii_lowercase().as_str() {
            "content-type" if self.content_type.is_none() => {
                self.content_type = Some(ContentType::parse(&field.body()));
            }
            "content-transfer-encoding" if self.transfer_encoding.is_none() => {
                let body = field.body();
                self.transfer_encoding = Some(crate::java::trim(&body).to_ascii_lowercase());
            }
            "content-disposition" if self.disposition.is_none() => {
                self.disposition = Some(field.body());
            }
            _ => {}
        }
    }

    fn in_digest(&self) -> bool {
        self.parent_mime_type
            .as_deref()
            .is_some_and(|parent| parent.eq_ignore_ascii_case("multipart/digest"))
    }

    fn build(&self) -> BodyDescriptor {
        let mut mime_type = None;
        let mut media_type = None;
        let mut subtype = None;
        let mut charset = None;
        let mut boundary = None;
        if let Some(content_type) = &self.content_type {
            mime_type = content_type.mime_type.clone();
            media_type = content_type.media_type.clone();
            subtype = content_type.subtype.clone();
            charset = content_type.parameter("charset").map(str::to_owned);
            boundary = content_type.parameter("boundary").map(str::to_owned);
            let multipart = media_type
                .as_deref()
                .is_some_and(|media| media.eq_ignore_ascii_case("multipart"));
            if multipart && boundary.is_none() {
                mime_type = None;
                media_type = None;
                subtype = None;
            }
        }
        let (mime_type, media_type, subtype) = match (mime_type, media_type, subtype) {
            (Some(mime), Some(media), Some(sub)) => (mime, media, sub),
            _ if self.in_digest() => ("message/rfc822".into(), "message".into(), "rfc822".into()),
            _ => ("text/plain".into(), "text".into(), "plain".into()),
        };
        if charset.is_none() && media_type == "text" {
            charset = Some("US-ASCII".to_owned());
        }
        if !is_multipart(&mime_type) {
            boundary = None;
        }
        BodyDescriptor {
            mime_type,
            media_type,
            subtype,
            boundary,
            charset,
            content_type: self.content_type.clone(),
            transfer_encoding: self.transfer_encoding.clone(),
            disposition: self.disposition.clone(),
        }
    }

    fn new_child(&self) -> DescriptorBuilder {
        let parent_mime_type = match &self.content_type {
            Some(content_type) => content_type.mime_type.clone(),
            None if self.in_digest() => Some("message/rfc822".to_owned()),
            None => Some("text/plain".to_owned()),
        };
        DescriptorBuilder {
            parent_mime_type,
            ..DescriptorBuilder::default()
        }
    }
}

/// `MimeUtil.isMultipart`.
fn is_multipart(mime_type: &str) -> bool {
    mime_type.to_lowercase().starts_with("multipart/")
}

/// A source of bytes as `InputStream.read(b, off, len)`: the count read, or `None` at the end.
type Source<'a> = dyn FnMut(&mut [u8]) -> Option<usize> + 'a;

/// `BufferedLineReaderInputStream`.
#[derive(Debug)]
struct LineBuffer {
    buf: Vec<u8>,
    pos: usize,
    limit: usize,
    truncated: bool,
}

impl LineBuffer {
    fn new() -> Self {
        LineBuffer {
            buf: vec![0; BUFFER_SIZE],
            pos: 0,
            limit: 0,
            truncated: false,
        }
    }

    fn ensure_capacity(&mut self, capacity: usize) {
        if capacity > self.buf.len() {
            self.buf.resize(capacity, 0);
        }
    }

    fn buffered(&self) -> usize {
        self.limit - self.pos
    }

    fn has_buffered_data(&self) -> bool {
        self.buffered() > 0
    }

    /// `fillBuffer`: compacts the buffer and reads once into the room after its data.
    fn fill(&mut self, source: &mut Source) -> Option<usize> {
        if self.pos > 0 {
            self.buf.copy_within(self.pos..self.limit, 0);
            self.limit -= self.pos;
            self.pos = 0;
        }
        let read = source(&mut self.buf[self.limit..])?;
        self.limit += read;
        Some(read)
    }

    fn read(&mut self, out: &mut [u8], source: &mut Source) -> Option<usize> {
        if self.truncated {
            return None;
        }
        while !self.has_buffered_data() {
            self.fill(source)?;
        }
        let chunk = self.buffered().min(out.len());
        out[..chunk].copy_from_slice(&self.buf[self.pos..self.pos + chunk]);
        self.pos += chunk;
        Some(chunk)
    }

    fn read_line(
        &mut self,
        line: &mut Vec<u8>,
        source: &mut Source,
    ) -> Result<Option<usize>, MimeError> {
        if self.truncated {
            return Ok(None);
        }
        let mut total = 0;
        let mut last_read = Some(0);
        loop {
            if !self.has_buffered_data() {
                last_read = self.fill(source);
                if last_read.is_none() {
                    break;
                }
            }
            let window = &self.buf[self.pos..self.limit];
            let (chunk, found) = match window.iter().position(|&b| b == b'\n') {
                Some(index) => (index + 1, true),
                None => (window.len(), false),
            };
            line.extend_from_slice(&window[..chunk]);
            self.pos += chunk;
            total += chunk;
            // mime4j fails a line of exactly the limit too (#78).
            if line.len() > MAX_LINE_LENGTH {
                return Err(MimeError(format!(
                    "Maximum line length limit ({MAX_LINE_LENGTH}) exceeded"
                )));
            }
            if found {
                break;
            }
        }
        Ok(if total == 0 && last_read.is_none() {
            None
        } else {
            Some(total)
        })
    }

    fn skip(&mut self, count: usize) {
        self.pos += count.min(self.buffered());
    }

    fn byte_at(&self, index: usize) -> u8 {
        self.buf[index]
    }

    /// `indexOf(pattern, off, len)`: its Boyer-Moore-Horspool search, which may stop at the end
    /// of the backing array before the window ends.
    fn index_of(&self, pattern: &[u8], from: usize, to: usize) -> Option<usize> {
        if to - from < pattern.len() {
            return None;
        }
        let mut shift = [pattern.len() + 1; 256];
        for (i, &b) in pattern.iter().enumerate() {
            shift[usize::from(b)] = pattern.len() - i;
        }
        let mut j = 0;
        while j <= to - from - pattern.len() {
            let current = from + j;
            if self.buf[current..current + pattern.len()] == *pattern {
                return Some(current);
            }
            let next = current + pattern.len();
            if next >= self.buf.len() {
                break;
            }
            j += shift[usize::from(self.buf[next])];
        }
        None
    }
}

/// `MimeBoundaryInputStream`: the data of one part, up to the line that starts with the
/// boundary.
#[derive(Debug)]
struct Boundary {
    pattern: Vec<u8>,
    eof: bool,
    /// Where the part's data ends in the buffer; negative or past the buffer's data when none
    /// of it is known to be the part's.
    limit: isize,
    at_boundary: bool,
    boundary_length: usize,
    last_part: bool,
    completed: bool,
    initial_length: isize,
    /// Whether the last byte the stream read ended a line, or it read none.
    line_start: bool,
}

impl Boundary {
    fn new(boundary: &str, buffer: &mut LineBuffer, source: &mut Source) -> Self {
        let mut pattern = b"--".to_vec();
        pattern.extend(boundary.encode_utf16().map(|unit| unit as u8));
        buffer.ensure_capacity((2 * crate::java::utf16_length(boundary)).max(BUFFER_SIZE));
        let mut stream = Boundary {
            pattern,
            eof: false,
            limit: -1,
            at_boundary: false,
            boundary_length: 0,
            last_part: false,
            completed: false,
            initial_length: -1,
            line_start: true,
        };
        stream.fill_buffer(buffer, source);
        stream
    }

    fn has_data(&self, buffer: &LineBuffer) -> bool {
        self.limit > buffer.pos as isize && self.limit <= buffer.limit as isize
    }

    fn end_of_stream(&self) -> bool {
        self.eof || self.at_boundary
    }

    fn read_allowed(&mut self, buffer: &mut LineBuffer, source: &mut Source) -> bool {
        if self.completed {
            return false;
        }
        if self.end_of_stream() && !self.has_data(buffer) {
            self.skip_boundary(buffer, source);
            return false;
        }
        true
    }

    fn read(
        &mut self,
        out: &mut [u8],
        buffer: &mut LineBuffer,
        source: &mut Source,
    ) -> Option<usize> {
        loop {
            if !self.read_allowed(buffer, source) {
                return None;
            }
            if self.has_data(buffer) {
                let chunk = out.len().min(self.limit as usize - buffer.pos);
                let read = buffer.read(&mut out[..chunk], source);
                if let Some(count @ 1..) = read {
                    self.line_start = out[count - 1] == b'\n';
                }
                return read;
            }
            self.fill_buffer(buffer, source);
        }
    }

    fn fill_buffer(&mut self, buffer: &mut LineBuffer, source: &mut Source) -> Option<usize> {
        if self.eof {
            return None;
        }
        let read = if self.has_data(buffer) {
            Some(0)
        } else {
            let read = buffer.fill(source);
            if read.is_none() {
                self.eof = true;
            }
            read
        };
        let found = self.find(buffer);
        match found {
            Some(index) => {
                self.limit = index as isize;
                self.at_boundary = true;
                self.calculate_boundary_length(buffer);
            }
            None if self.eof => self.limit = buffer.limit as isize,
            None => {
                let reserve = self.pattern.len() as isize + 3;
                let position = buffer.limit as isize - reserve;
                self.limit = if position >= 0 && buffer.byte_at(position as usize) == b'\r' {
                    position
                } else {
                    buffer.limit as isize - (self.pattern.len() as isize + 2)
                };
            }
        }
        read
    }

    /// The boundary at the start of a line, followed by white space, `--` or the end of the
    /// part. One the buffer ends too soon after to tell is not found until the buffer holds what
    /// follows it: the data held back before it keeps it there.
    fn find(&self, buffer: &LineBuffer) -> Option<usize> {
        let mut from = buffer.pos;
        loop {
            let index = buffer.index_of(&self.pattern, from, buffer.limit)?;
            let line_start = if index == buffer.pos {
                self.line_start
            } else {
                buffer.byte_at(index - 1) == b'\n'
            };
            if line_start {
                let after = index + self.pattern.len();
                if after >= buffer.limit {
                    return self.eof.then_some(index);
                }
                let next = buffer.byte_at(after);
                if is_whitespace(next) {
                    return Some(index);
                }
                if next == b'-' {
                    if buffer.limit - after > 1 {
                        if buffer.byte_at(after + 1) == b'-' {
                            return Some(index);
                        }
                    } else if !self.eof {
                        return None;
                    }
                }
            }
            from = index + self.pattern.len();
        }
    }

    fn calculate_boundary_length(&mut self, buffer: &LineBuffer) {
        self.boundary_length = self.pattern.len();
        let length = self.limit - buffer.pos as isize;
        if length >= 0 && self.initial_length == -1 {
            self.initial_length = length;
        }
        if length > 0 && buffer.byte_at(self.limit as usize - 1) == b'\n' {
            self.boundary_length += 1;
            self.limit -= 1;
        }
        if length > 1 && buffer.byte_at(self.limit as usize - 1) == b'\r' {
            self.boundary_length += 1;
            self.limit -= 1;
        }
    }

    fn skip_boundary(&mut self, buffer: &mut LineBuffer, source: &mut Source) {
        if self.completed {
            return;
        }
        self.completed = true;
        buffer.skip(self.boundary_length);
        let mut check_for_last_part = true;
        loop {
            if buffer.buffered() > 1 {
                let first = buffer.byte_at(buffer.pos);
                let second = buffer.byte_at(buffer.pos + 1);
                if check_for_last_part && first == b'-' && second == b'-' {
                    self.last_part = true;
                    buffer.skip(2);
                    check_for_last_part = false;
                    continue;
                }
                if first == b'\r' && second == b'\n' {
                    buffer.skip(2);
                    break;
                } else if first == b'\n' {
                    buffer.skip(1);
                    break;
                }
                buffer.skip(1);
            } else {
                if self.eof {
                    break;
                }
                self.fill_buffer(buffer, source);
            }
        }
    }

    fn is_empty_stream(&self) -> bool {
        self.initial_length == 0
    }

    fn is_fully_consumed(&self, buffer: &LineBuffer) -> bool {
        self.completed && !buffer.has_buffered_data()
    }

    fn at_eof(&self, buffer: &LineBuffer) -> bool {
        self.eof && !buffer.has_buffered_data()
    }
}

/// `MimeEntity`'s states, `EntityState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    StartMessage,
    StartBodyPart,
    StartHeader,
    Field,
    EndHeader,
    StartMultipart,
    Preamble,
    Epilogue,
    Body,
    EndMultipart,
    EndBodyPart,
    EndMessage,
    EndOfStream,
}

/// What an entity's data stream, its `LineReaderInputStreamAdaptor`, reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Over {
    Buffer,
    Part,
}

/// `LineReaderInputStreamAdaptor`'s state.
#[derive(Debug, Clone, Copy)]
struct DataStream {
    over: Over,
    used: bool,
    eof: bool,
}

impl DataStream {
    fn over(over: Over) -> Self {
        DataStream {
            over,
            used: false,
            eof: false,
        }
    }
}

/// `MimeEntity`.
#[derive(Debug)]
struct Entity {
    buffer: LineBuffer,
    part: Option<Boundary>,
    data: DataStream,
    state: State,
    end_state: State,
    line: Vec<u8>,
    line_count: usize,
    end_of_header: bool,
    header_count: usize,
    field: Option<RawField>,
    body: Option<BodyDescriptor>,
    builder: DescriptorBuilder,
}

impl Entity {
    fn new(start: State, end_state: State, builder: DescriptorBuilder) -> Self {
        Entity {
            buffer: LineBuffer::new(),
            part: None,
            data: DataStream::over(Over::Buffer),
            state: start,
            end_state,
            line: Vec::with_capacity(64),
            line_count: 0,
            end_of_header: false,
            header_count: 0,
            field: None,
            body: None,
            builder,
        }
    }

    fn body(&self) -> &BodyDescriptor {
        self.body
            .as_ref()
            .expect("an entity has a body descriptor once its header ends")
    }
}

/// The root of the chain: the message's bytes, read in full as a `TikaInputStream` reads.
struct Root<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Root<'_> {
    fn read(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.pos >= self.input.len() {
            return if out.is_empty() { Some(0) } else { None };
        }
        let count = out.len().min(self.input.len() - self.pos);
        out[..count].copy_from_slice(&self.input[self.pos..self.pos + count]);
        self.pos += count;
        Some(count)
    }
}

/// Reads into `out` from the source of the entity after `entities`: the message itself, or the
/// current part of the last of `entities`.
fn read_source(root: &mut Root, entities: &mut [Entity], out: &mut [u8]) -> Option<usize> {
    let Some((parent, ancestors)) = entities.split_last_mut() else {
        return root.read(out);
    };
    let Entity { buffer, part, .. } = parent;
    let part = part
        .as_mut()
        .expect("a part's entity reads from its parent's current part");
    part.read(out, buffer, &mut |into: &mut [u8]| {
        read_source(root, ancestors, into)
    })
}

/// Reads through the data stream of the last of `entities`.
fn read_data(root: &mut Root, entities: &mut [Entity], out: &mut [u8]) -> Option<usize> {
    let (entity, ancestors) = entities
        .split_last_mut()
        .expect("data is read from an entity");
    let mut source = |into: &mut [u8]| read_source(root, ancestors, into);
    let read = match entity.data.over {
        Over::Buffer => entity.buffer.read(out, &mut source),
        Over::Part => entity
            .part
            .as_mut()
            .expect("a data stream over a part has a part")
            .read(out, &mut entity.buffer, &mut source),
    };
    entity.data.eof = read.is_none();
    entity.data.used = true;
    read
}

/// Reads the messages's parts as `MimeStreamParser.parse` reports them.
pub(super) fn events(input: &[u8]) -> Result<Vec<Event>, MimeError> {
    let mut stream = Stream {
        root: Root { input, pos: 0 },
        entities: vec![Entity::new(
            State::StartMessage,
            State::EndMessage,
            DescriptorBuilder::default(),
        )],
        field_bytes: Vec::new(),
        part_count: 0,
        total_header_count: 0,
        total_header_length: 0,
    };
    let mut events = Vec::new();
    let mut state = State::StartMessage;
    loop {
        match state {
            State::Body => {
                let body = stream.current().body().clone();
                let content = stream.body_content(&body)?;
                events.push(Event::Body(body, content));
            }
            State::EndBodyPart => events.push(Event::EndBodyPart),
            State::EndMultipart => events.push(Event::EndMultipart),
            State::Field => {
                let field = stream
                    .current()
                    .field
                    .clone()
                    .expect("an entity in its field state has a field");
                events.push(Event::Field(field));
            }
            State::StartBodyPart => events.push(Event::StartBodyPart),
            State::StartMultipart => {
                events.push(Event::StartMultipart(stream.current().body().clone()));
            }
            State::EndOfStream => return Ok(events),
            _ => {}
        }
        state = stream.next()?;
    }
}

struct Stream<'a> {
    root: Root<'a>,
    entities: Vec<Entity>,
    /// The shared `DefaultFieldBuilder`'s bytes.
    field_bytes: Vec<u8>,
    part_count: usize,
    total_header_count: usize,
    total_header_length: usize,
}

impl Stream<'_> {
    fn current(&self) -> &Entity {
        self.entities.last().expect("the stream has an entity")
    }

    fn current_mut(&mut self) -> &mut Entity {
        self.entities.last_mut().expect("the stream has an entity")
    }

    /// `MimeTokenStream.next`.
    fn next(&mut self) -> Result<State, MimeError> {
        while !self.entities.is_empty() {
            if let Some(child) = self.advance()? {
                if self.part_count >= MAX_PART_COUNT {
                    return Err(MimeError(format!(
                        "Maximum part count limit ({MAX_PART_COUNT}) exceeded"
                    )));
                }
                self.part_count += 1;
                if self.entities.len() >= MAX_NESTING_DEPTH {
                    return Err(MimeError(format!(
                        "Maximum nesting depth limit ({MAX_NESTING_DEPTH}) exceeded"
                    )));
                }
                self.entities.push(child);
            }
            let state = self.current().state;
            if state != State::EndOfStream {
                if state == State::Field {
                    self.check_total_header_limits()?;
                }
                return Ok(state);
            }
            let mut finished = self.entities.pop().expect("the stream has an entity");
            finished.buffer.pos = 0;
            finished.buffer.limit = 0;
            finished.buffer.truncated = true;
        }
        Ok(State::EndOfStream)
    }

    fn check_total_header_limits(&mut self) -> Result<(), MimeError> {
        self.total_header_count += 1;
        if self.total_header_count > MAX_TOTAL_HEADER_COUNT {
            return Err(MimeError(format!(
                "Maximum total header limit ({MAX_TOTAL_HEADER_COUNT}) exceeded"
            )));
        }
        let field = self
            .current()
            .field
            .as_ref()
            .expect("a field was just read");
        self.total_header_length += field.raw().len();
        if self.total_header_length > MAX_TOTAL_HEADER_LENGTH {
            return Err(MimeError(format!(
                "Maximum total header length limit ({MAX_TOTAL_HEADER_LENGTH}) exceeded"
            )));
        }
        Ok(())
    }

    /// `MimeEntity.advance` of the current entity, with the part entity it starts, if any.
    fn advance(&mut self) -> Result<Option<Entity>, MimeError> {
        let mut state = self.current().state;
        loop {
            match state {
                State::StartMessage | State::StartBodyPart => {
                    self.current_mut().state = State::StartHeader;
                    return Ok(None);
                }
                State::StartHeader => {
                    self.current_mut().builder.reset();
                    state = State::Field;
                }
                State::Field => {
                    let next = if self.next_field()? {
                        State::Field
                    } else {
                        State::EndHeader
                    };
                    self.current_mut().state = next;
                    return Ok(None);
                }
                State::EndHeader => {
                    let entity = self.current_mut();
                    let body = entity.builder.build();
                    let multipart = is_multipart(&body.mime_type);
                    entity.body = Some(body);
                    if multipart {
                        entity.state = State::StartMultipart;
                        self.clear_part();
                    } else {
                        entity.state = State::Body;
                    }
                    return Ok(None);
                }
                State::StartMultipart => {
                    if self.current().data.used {
                        self.advance_to_boundary();
                        self.current_mut().state = State::EndMultipart;
                        return Ok(None);
                    }
                    self.create_part();
                    self.current_mut().state = State::Preamble;
                    let empty = self
                        .current()
                        .part
                        .as_ref()
                        .is_some_and(Boundary::is_empty_stream);
                    if !empty {
                        return Ok(None);
                    }
                    state = State::Preamble;
                }
                State::Preamble => {
                    self.advance_to_boundary();
                    let entity = self.current();
                    let part = entity.part.as_ref().expect("a multipart reads a part");
                    let premature = part.at_eof(&entity.buffer) && !part.last_part;
                    if !premature && !part.last_part {
                        self.clear_part();
                        self.create_part();
                        let child = Entity::new(
                            State::StartBodyPart,
                            State::EndBodyPart,
                            self.current().builder.new_child(),
                        );
                        return Ok(Some(child));
                    }
                    let entity = self.current();
                    let empty = entity
                        .part
                        .as_ref()
                        .expect("a multipart reads a part")
                        .is_fully_consumed(&entity.buffer);
                    self.clear_part();
                    self.current_mut().state = State::Epilogue;
                    if !empty {
                        return Ok(None);
                    }
                    state = State::Epilogue;
                }
                State::Epilogue => {
                    self.current_mut().state = State::EndMultipart;
                    return Ok(None);
                }
                State::Body | State::EndMultipart => {
                    let entity = self.current_mut();
                    entity.state = entity.end_state;
                    return Ok(None);
                }
                State::EndBodyPart | State::EndMessage | State::EndOfStream => {
                    let entity = self.current_mut();
                    if entity.state != entity.end_state {
                        unreachable!("an entity advances past its end only once");
                    }
                    entity.state = State::EndOfStream;
                    return Ok(None);
                }
            }
        }
    }

    fn create_part(&mut self) {
        let (entity, ancestors) = self
            .entities
            .split_last_mut()
            .expect("the stream has an entity");
        let boundary = entity
            .body()
            .boundary
            .clone()
            .expect("a multipart's descriptor has a boundary");
        let root = &mut self.root;
        let part = Boundary::new(&boundary, &mut entity.buffer, &mut |into: &mut [u8]| {
            read_source(root, ancestors, into)
        });
        entity.part = Some(part);
        entity.data = DataStream::over(Over::Part);
    }

    fn clear_part(&mut self) {
        let entity = self.current_mut();
        entity.part = None;
        entity.data = DataStream::over(Over::Buffer);
    }

    /// `advanceToBoundary`: drains the data stream unless it is at its end.
    fn advance_to_boundary(&mut self) {
        if self.current().data.eof {
            return;
        }
        let mut drain = vec![0; DRAIN_BUFFER_SIZE];
        while read_data(&mut self.root, &mut self.entities, &mut drain).is_some() {}
    }

    /// `nextField`: the next valid field of the header, if any.
    fn next_field(&mut self) -> Result<bool, MimeError> {
        loop {
            if self.current().end_of_header {
                return Ok(false);
            }
            if self.current().header_count >= MAX_HEADER_COUNT {
                return Err(MimeError(format!(
                    "Maximum header limit ({MAX_HEADER_COUNT}) exceeded"
                )));
            }
            self.current_mut().header_count += 1;
            self.field_bytes.clear();
            self.read_raw_field()?;
            if let Ok(field) = RawField::parse(self.field_bytes.clone()) {
                let entity = self.current_mut();
                entity.builder.add_field(&field);
                entity.field = Some(field);
                return Ok(true);
            }
        }
    }

    /// `readRawField`: the lines of one field into the field builder.
    fn read_raw_field(&mut self) -> Result<(), MimeError> {
        let (entity, ancestors) = self
            .entities
            .split_last_mut()
            .expect("the stream has an entity");
        let root = &mut self.root;
        let mut source = |into: &mut [u8]| read_source(root, ancestors, into);
        loop {
            if !entity.line.is_empty() {
                // mime4j fails a field of exactly the limit too (#78).
                if self.field_bytes.len() + entity.line.len() > MAX_HEADER_LENGTH {
                    return Err(MimeError(format!(
                        "Maximum header length limit ({MAX_HEADER_LENGTH}) exceeded"
                    )));
                }
                self.field_bytes.extend_from_slice(&entity.line);
            }
            entity.line.clear();
            let read = entity.buffer.read_line(&mut entity.line, &mut source)?;
            entity.data.eof = read.is_none();
            entity.data.used = true;
            if read.is_none() {
                entity.end_of_header = true;
                return Ok(());
            }
            let mut length = entity.line.len();
            if length > 0 && entity.line[length - 1] == b'\n' {
                length -= 1;
            }
            if length > 0 && entity.line[length - 1] == b'\r' {
                length -= 1;
            }
            if length == 0 {
                entity.end_of_header = true;
                return Ok(());
            }
            entity.line_count += 1;
            if entity.line_count > 1 && !matches!(entity.line[0], b' ' | b'\t') {
                return Ok(());
            }
        }
    }

    /// The body of the current entity as the handler reads it, its transfer encoding decoded.
    fn body_content(&mut self, body: &BodyDescriptor) -> Result<Vec<u8>, MimeError> {
        let encoding = body.transfer_encoding();
        let mut raw = Vec::new();
        let mut chunk = vec![0; COPY_BUFFER_SIZE];
        while let Some(count) = read_data(&mut self.root, &mut self.entities, &mut chunk) {
            raw.extend_from_slice(&chunk[..count]);
        }
        Ok(if encoding.eq_ignore_ascii_case("base64") {
            decode_base64(&raw)
        } else if encoding.eq_ignore_ascii_case("quoted-printable") {
            decode_quoted_printable(&raw)
        } else {
            raw
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(input: &[u8]) -> Vec<String> {
        events(input)
            .unwrap()
            .into_iter()
            .map(|event| match event {
                Event::Field(field) => format!("field {}", field.name()),
                Event::StartMultipart(body) => format!("multipart {}", body.mime_type),
                Event::EndMultipart => "end multipart".into(),
                Event::StartBodyPart => "part".into(),
                Event::EndBodyPart => "end part".into(),
                Event::Body(body, content) => format!(
                    "body {} {:?}",
                    body.mime_type,
                    String::from_utf8_lossy(&content)
                ),
            })
            .collect()
    }

    #[test]
    fn reads_a_single_part_message() {
        let message = b"Subject: hi\r\n  there\r\nbogus line\r\nContent-Transfer-Encoding: base64\r\n\r\naGk=\r\n";
        assert_eq!(
            summary(message),
            [
                "field Subject",
                "field Content-Transfer-Encoding",
                "body text/plain \"hi\""
            ]
        );
    }

    #[test]
    fn reads_multipart_messages_as_mime4j_does() {
        let message = b"Content-Type: multipart/mixed; boundary=b\r\n\r\npreamble\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n--b \r\n\r\nplain\n--b--\r\nepilogue";
        assert_eq!(
            summary(message),
            [
                "field Content-Type",
                "multipart multipart/mixed",
                "part",
                "field Content-Type",
                "body text/html \"<p>x</p>\"",
                "end part",
                "part",
                "body text/plain \"plain\"",
                "end part",
                "end multipart",
            ]
        );
    }

    #[test]
    fn finds_boundaries_at_the_start_of_lines_alone() {
        for filler in (4000..4200).chain(8100..8200) {
            let message = format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n\r\n{}--b inside\r\n--b-\r\n--b--\r\n",
                "y".repeat(filler)
            );
            let body = format!("{}--b inside\r\n--b-", "y".repeat(filler));
            assert_eq!(
                summary(message.as_bytes()),
                [
                    "field Content-Type".to_owned(),
                    "multipart multipart/mixed".to_owned(),
                    "part".to_owned(),
                    format!("body text/plain {body:?}"),
                    "end part".to_owned(),
                    "end multipart".to_owned(),
                ],
                "{filler} bytes before the boundary"
            );
        }
    }

    #[test]
    fn treats_a_multipart_without_boundary_as_text() {
        let message = b"Content-Type: multipart/mixed\r\n\r\n--b\r\nx";
        assert_eq!(
            summary(message),
            ["field Content-Type", "body text/plain \"--b\\r\\nx\""]
        );
    }

    #[test]
    fn fails_on_lines_past_the_limit() {
        let mut message = b"Subject: ".to_vec();
        message.extend(std::iter::repeat_n(b'a', MAX_LINE_LENGTH));
        assert!(events(&message).is_err());
    }
}
