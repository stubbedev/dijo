//! jai-imageio's `PNMImageReader` as far as its header, and `PNMMetadata`'s standard tree: its
//! native tree keeps values in its nodes rather than in attributes, so Tika takes nothing from
//! it. ImageIO picks that reader for portable pixmaps, the one portable map Tika's `ImageParser`
//! reads.

use super::{standard_tree, Eof, Node, Read, Stream};

/// Why the portable map could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PnmError {
    /// The `RuntimeException` of a header that is not a portable map's.
    NotPnm,
    /// The `EOFException` of a header that ends before its variant.
    Eof,
}

impl From<Eof> for PnmError {
    fn from(_: Eof) -> Self {
        Self::Eof
    }
}

/// The portable map's kind, of the digit after its `P`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bitmap,
    Graymap,
    Pixmap,
}

/// `getNumImages(true)`, which is one without reading, then `getWidth(0)`, `getHeight(0)` and
/// `getImageMetadata(0)`, which read the header once.
pub(crate) fn read(input: &[u8]) -> Result<Read, PnmError> {
    let mut stream = Stream::new(input);
    if stream.u8()? != b'P' {
        return Err(PnmError::NotPnm);
    }
    let kind = match stream.u8()? {
        b'1' | b'4' => Kind::Bitmap,
        b'2' | b'5' => Kind::Graymap,
        b'3' | b'6' => Kind::Pixmap,
        _ => return Err(PnmError::NotPnm),
    };
    stream.read_line();
    let comments = comments(&mut stream);
    let mut integers = Integers::default();
    let width = integers.next(&mut stream);
    let height = integers.next(&mut stream);
    let max_value = match kind {
        Kind::Bitmap => 1,
        Kind::Graymap | Kind::Pixmap => integers.next(&mut stream),
    };
    Ok(Read {
        images: 1,
        width,
        height,
        trees: vec![standard_tree([
            Some(chroma(kind)),
            None,
            Some(data(kind, max_sample_size(max_value))),
            Some(
                Node::new("Dimension")
                    .child(Node::new("ImageOrientation").attribute("value", "Normal")),
            ),
            None,
            text(comments),
            None,
            None,
        ])],
    })
}

/// `readComments`: what follows the `#` of each line from here that has one, trimmed, up to the
/// first that has none, read without moving the stream.
fn comments(stream: &mut Stream) -> Vec<String> {
    let mark = stream.position;
    let mut comments = Vec::new();
    while let Some(line) = stream.read_line() {
        let Some(hash) = line.find('#') else {
            break;
        };
        let comment = crate::java::trim(&line[hash + 1..]);
        comments.push(comment.replace(['\n', '\r', '\u{c}'], " "));
    }
    stream.position = mark;
    comments
}

/// `readInteger`'s state: the tokens left of the line it read last, if any.
#[derive(Default)]
struct Integers {
    tokens: Option<std::vec::IntoIter<String>>,
}

impl Integers {
    /// `readInteger`: the next token that is an `int`, of lines cut at the character before
    /// their first `#`, lines that start with one skipped; zero at the end of the stream.
    fn next(&mut self, stream: &mut Stream) -> i32 {
        loop {
            if self.tokens.is_none() {
                let Some(line) = stream.read_line() else {
                    return 0;
                };
                let line = match line.find('#') {
                    Some(0) => continue,
                    Some(hash) => {
                        let mut kept = line[..hash].chars();
                        kept.next_back();
                        kept.as_str()
                    }
                    None => &line[..],
                };
                let tokens: Vec<String> = line
                    .split([' ', '\t', '\n', '\r', '\u{c}'])
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
                    .collect();
                self.tokens = Some(tokens.into_iter());
            }
            if let Some(value) = self
                .tokens
                .as_mut()
                .and_then(|tokens| tokens.find_map(|token| crate::java::parse_int(&token)))
            {
                return value;
            }
            self.tokens = None;
        }
    }
}

/// `setMaxBitDepth`: the number of bits of the largest sample, none of one that is not positive.
fn max_sample_size(max_value: i32) -> u32 {
    if max_value > 0 {
        i32::BITS - max_value.leading_zeros()
    } else {
        0
    }
}

/// `getStandardChromaNode`.
fn chroma(kind: Kind) -> Node {
    let (space, channels) = match kind {
        Kind::Pixmap => ("RGB", "3"),
        Kind::Bitmap | Kind::Graymap => ("GRAY", "1"),
    };
    let node = Node::new("Chroma")
        .child(Node::new("ColorSpaceType").attribute("name", space))
        .child(Node::new("NumChannels").attribute("value", channels));
    match kind {
        Kind::Pixmap => node,
        Kind::Bitmap | Kind::Graymap => {
            node.child(Node::new("BlackIsZero").attribute("value", "TRUE"))
        }
    }
}

/// `getStandardDataNode`.
fn data(kind: Kind, max_sample_size: u32) -> Node {
    let (bits, significant) = match kind {
        Kind::Bitmap => ("1", max_sample_size.to_string()),
        Kind::Graymap => ("8", max_sample_size.to_string()),
        Kind::Pixmap => (
            "8 8 8",
            format!("{max_sample_size} {max_sample_size} {max_sample_size}"),
        ),
    };
    Node::new("Data")
        .child(Node::new("SampleFormat").attribute("value", "UnsignedIntegral"))
        .child(Node::new("BitsPerSample").attribute("value", bits))
        .child(Node::new("SignificantBitsPerSample").attribute("value", significant))
}

/// `getStandardTextNode`: an entry of each comment, none without comments.
fn text(comments: Vec<String>) -> Option<Node> {
    if comments.is_empty() {
        return None;
    }
    let mut node = Node::new("Text");
    for comment in comments {
        node.push(
            Node::new("TextEntry")
                .attribute("keyword", "comment")
                .attribute("value", comment),
        );
    }
    Some(node)
}
