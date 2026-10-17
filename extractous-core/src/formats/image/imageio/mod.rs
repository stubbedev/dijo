//! The image metadata Tika's `ImageParser` takes from Java's ImageIO: the reader ImageIO picks
//! for the type reads the image's header and metadata, whose trees (the reader's native format,
//! then `javax_imageio_1.0`) Tika walks, keeping each node's attributes under `img:` and the
//! names of the nodes above it.

mod creation_time;
pub(crate) mod gif;
pub(crate) mod jbig2;
pub(crate) mod png;

use super::extractor::{Target, UNKNOWN_IMG};

/// An `IIOMetadataNode`: a name, attributes in the order they were set, and children. A node's
/// value and user object are not kept: Tika reads attributes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    name: &'static str,
    attributes: Vec<(&'static str, String)>,
    children: Vec<Node>,
}

impl Node {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// `setAttribute`: a new attribute after the others, a known one replaced in its place.
    pub(crate) fn attribute(mut self, name: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        match self.attributes.iter_mut().find(|(known, _)| *known == name) {
            Some((_, slot)) => *slot = value,
            None => self.attributes.push((name, value)),
        }
        self
    }

    pub(crate) fn child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    pub(crate) fn push(&mut self, child: Node) {
        self.children.push(child);
    }
}

/// What a reader found: the image's size, the number of images, and the metadata trees.
#[derive(Debug, Clone)]
pub(crate) struct Read {
    pub(crate) images: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) trees: Vec<Node>,
}

/// `IIOMetadata.getStandardTree`: the standard nodes a format gives, in their order, with those
/// it does not give left out.
pub(crate) fn standard_tree(nodes: [Option<Node>; 8]) -> Node {
    let mut root = Node::new("javax_imageio_1.0");
    for node in nodes.into_iter().flatten() {
        root.push(node);
    }
    root
}

/// `ImageParser.loadNode`: each node's attributes under the names of the nodes from below the
/// root down to it, one attribute as its value, several as `name=value` pairs.
pub(crate) fn load_node(target: &mut Target, node: &Node, parents: &str, add_this: bool) {
    let mut parents = parents.to_owned();
    if add_this {
        if !parents.is_empty() {
            parents.push(' ');
        }
        parents.push_str(node.name);
    }
    if !parents.is_empty() {
        match node.attributes.as_slice() {
            [] => {}
            [(_, value)] => target.add_prefixed(UNKNOWN_IMG, &parents, &normalize(value)),
            attributes => {
                let value = attributes
                    .iter()
                    .map(|(name, value)| format!("{name}={}", normalize(value)))
                    .collect::<Vec<_>>()
                    .join(", ");
                target.add_prefixed(UNKNOWN_IMG, &parents, &value);
            }
        }
    }
    for child in &node.children {
        load_node(target, child, &parents, true);
    }
}

/// `ImageParser.normalize`: trimmed, and booleans in lower case.
fn normalize(value: &str) -> String {
    let value = crate::java::trim(value);
    if crate::java::equals_ignore_case(value, "true") {
        "true".to_owned()
    } else if crate::java::equals_ignore_case(value, "false") {
        "false".to_owned()
    } else {
        value.to_owned()
    }
}

/// An `ImageInputStream` over the image's bytes: a position that may pass the end, and reads
/// that fail with `EOFException` there.
#[derive(Debug, Clone)]
pub(crate) struct Stream<'a> {
    data: &'a [u8],
    pub(crate) position: u64,
}

/// `EOFException`, or another `IOException` a reader throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Eof;

impl<'a> Stream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// `read()`: the next byte, or none at the end.
    pub(crate) fn read(&mut self) -> Option<u8> {
        let byte = self.data.get(usize::try_from(self.position).ok()?).copied();
        if byte.is_some() {
            self.position += 1;
        }
        byte
    }

    /// `readFully`.
    pub(crate) fn bytes(&mut self, count: usize) -> Result<&'a [u8], Eof> {
        let start = usize::try_from(self.position).map_err(|_| Eof)?;
        let end = start.checked_add(count).ok_or(Eof)?;
        let bytes = self.data.get(start..end).ok_or(Eof)?;
        self.position += count as u64;
        Ok(bytes)
    }

    /// `read(byte[], off, len)`: as many bytes as are left, up to `count`.
    pub(crate) fn some_bytes(&mut self, count: usize) -> &'a [u8] {
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(self.data.len());
        let end = start.saturating_add(count).min(self.data.len());
        self.position = self.position.max(start as u64) + (end - start) as u64;
        &self.data[start..end]
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Eof> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn u16_be(&mut self) -> Result<u16, Eof> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn i32_be(&mut self) -> Result<i32, Eof> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn u16_le(&mut self) -> Result<u16, Eof> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// `skipBytes`, which may move past the end.
    pub(crate) fn skip(&mut self, count: u64) {
        self.position = self.position.saturating_add(count);
    }
}
