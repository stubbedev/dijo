//! HDF5 files, read in place for their metadata only: the superblock, the object headers of the
//! groups and datasets the root group reaches, and their attribute, datatype and dataspace
//! messages, compact or in dense storage. The data of the datasets is never read.

mod btree;
mod datatype;
mod heap;

use std::collections::BTreeSet;

use super::Contents;
use crate::formats::bytes::{malformed, unported, Bytes, Cursor, Order, Read};
use crate::formats::cdm::{Attribute, Variable};
use datatype::{values, Dataspace, Datatype};
use heap::FractalHeap;

/// The signature a superblock starts with.
pub(super) const SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

const DATASPACE: u16 = 0x0001;
const LINK_INFO: u16 = 0x0002;
const DATATYPE: u16 = 0x0003;
const LINK: u16 = 0x0006;
const LAYOUT: u16 = 0x0008;
const ATTRIBUTE: u16 = 0x000C;
const CONTINUATION: u16 = 0x0010;
const SYMBOL_TABLE: u16 = 0x0011;
const ATTRIBUTE_INFO: u16 = 0x0015;

/// A message's flag that it is shared, held elsewhere.
const SHARED: u8 = 0x02;

/// The file, as its superblock says to address it.
pub(super) struct File<'a> {
    bytes: Bytes<'a>,
    base: u64,
    offsets: usize,
    lengths: usize,
}

impl<'a> File<'a> {
    fn absolute(&self, address: u64, what: &str) -> Read<u64> {
        self.base
            .checked_add(address)
            .ok_or_else(|| malformed(format!("{what} at an address past any file: {address}")))
    }

    fn slice(&self, address: u64, length: u64, what: &str) -> Read<&'a [u8]> {
        self.bytes
            .slice(self.absolute(address, what)?, length, what)
    }

    fn cursor(&self, address: u64, length: u64, what: &'static str) -> Read<Cursor<'a>> {
        self.bytes
            .cursor(self.absolute(address, what)?, length, Order::Little, what)
    }

    fn cursor_from(&self, address: u64, what: &'static str) -> Read<Cursor<'a>> {
        self.bytes
            .cursor_from(self.absolute(address, what)?, Order::Little, what)
    }

    /// Whether `address` is the undefined address, all bits set.
    fn undefined(&self, address: u64) -> bool {
        address == u64::MAX >> (64 - 8 * self.offsets as u32)
    }
}

/// Reads what the root group reaches: the attributes of every group, the root's first, then
/// those of each group it holds, depth first, and every dataset as a variable, each group's
/// in the order of their names before those of the groups it holds.
pub(super) fn read(bytes: Bytes) -> Read<Contents> {
    let (file, root) = superblock(bytes)?;
    let mut contents = Contents::default();
    let mut visited = BTreeSet::from([root]);
    let mut pending = vec![(Object::read(&file, root)?, String::new())];
    while let Some((object, path)) = pending.pop() {
        contents.attributes.extend(object.attributes(&file)?);
        let mut groups = Vec::new();
        for (name, header) in object.links(&file)? {
            if !visited.insert(header) {
                continue;
            }
            let child_path = if path.is_empty() {
                name
            } else {
                format!("{path}/{name}")
            };
            let child = Object::read(&file, header)?;
            if child.is_group() {
                groups.push((child, child_path));
            } else if let Some(variable) = child.variable(&file, child_path)? {
                contents.variables.push(variable);
            }
        }
        pending.extend(groups.into_iter().rev());
    }
    Ok(contents)
}

/// The superblock: how the file is addressed, and its root group's object header.
fn superblock(bytes: Bytes) -> Read<(File, u64)> {
    let mut at = 0u64;
    let start = loop {
        if at >= bytes.len() {
            return Err(malformed(
                "no HDF5 superblock signature at any offset it may stand",
            ));
        }
        if bytes.slice(at, 8, "a superblock signature").ok() == Some(SIGNATURE.as_slice()) {
            break at;
        }
        at = if at == 0 { 512 } else { at * 2 };
    };
    let mut cursor = bytes.cursor_from(start + 8, Order::Little, "the HDF5 superblock")?;
    let version = cursor.u8()?;
    let (offsets, lengths) = match version {
        0 | 1 => {
            cursor.skip(4)?;
            let sizes = (usize::from(cursor.u8()?), usize::from(cursor.u8()?));
            cursor.skip(1 + 4 + 4)?;
            if version == 1 {
                cursor.skip(4)?;
            }
            sizes
        }
        2 | 3 => {
            let sizes = (usize::from(cursor.u8()?), usize::from(cursor.u8()?));
            cursor.skip(1)?;
            sizes
        }
        _ => return Err(cursor.malformed(format!("a superblock of unknown version {version}"))),
    };
    if ![2, 4, 8].contains(&offsets) || ![2, 4, 8].contains(&lengths) {
        return Err(cursor.malformed(format!(
            "addresses of {offsets} bytes and lengths of {lengths}"
        )));
    }
    let base = cursor.uint(offsets)?;
    let root = if version < 2 {
        cursor.skip(3 * offsets)?;
        cursor.skip(offsets)?;
        cursor.uint(offsets)?
    } else {
        cursor.skip(2 * offsets)?;
        cursor.uint(offsets)?
    };
    let file = File {
        bytes,
        base,
        offsets,
        lengths,
    };
    Ok((file, root))
}

/// A message of an object header: its type, flags and body.
struct Message<'a> {
    kind: u16,
    flags: u8,
    body: Cursor<'a>,
}

/// The messages of an object header.
struct Object<'a> {
    address: u64,
    messages: Vec<Message<'a>>,
}

impl<'a> Object<'a> {
    fn read(file: &File<'a>, address: u64) -> Read<Self> {
        let signature = file.slice(address, 4, "an object header")?;
        let messages = if signature == b"OHDR" {
            version_2(file, address)?
        } else {
            version_1(file, address)?
        };
        Ok(Self { address, messages })
    }

    fn find(&self, kind: u16) -> Option<&Message<'a>> {
        self.messages.iter().find(|message| message.kind == kind)
    }

    fn is_group(&self) -> bool {
        self.messages
            .iter()
            .any(|message| matches!(message.kind, SYMBOL_TABLE | LINK_INFO | LINK))
    }

    /// The hard links of a group, each by its name and the object header it links to, in the
    /// order of their names.
    fn links(&self, file: &File<'a>) -> Read<Vec<(String, u64)>> {
        let mut links = Vec::new();
        for message in &self.messages {
            match message.kind {
                SYMBOL_TABLE => {
                    let mut body = message.body.clone();
                    let tree = body.uint(file.offsets)?;
                    let names = heap::local_heap(file, body.uint(file.offsets)?)?;
                    for symbol in btree::symbols(file, tree)? {
                        if let Some(header) = symbol.header {
                            links.push((heap::heap_name(names, symbol.name_offset)?, header));
                        }
                    }
                }
                LINK => links.extend(link(file, &mut message.body.clone())?),
                LINK_INFO => {
                    let mut body = message.body.clone();
                    body.u8()?;
                    let flags = body.u8()?;
                    if flags & 0x01 != 0 {
                        body.skip(8)?;
                    }
                    let heap = body.uint(file.offsets)?;
                    let index = body.uint(file.offsets)?;
                    if file.undefined(heap) || file.undefined(index) {
                        continue;
                    }
                    let heap = FractalHeap::read(file, heap)?;
                    for record in btree::records(file, index)? {
                        let id = record.get(4..).ok_or_else(|| {
                            malformed(format!("a link name record of {} bytes", record.len()))
                        })?;
                        let id = &id[..heap.heap_id_length().min(id.len())];
                        let mut encoded = Cursor::new(
                            heap.object(file, id)?,
                            0,
                            Order::Little,
                            "a link in dense storage",
                        );
                        links.extend(link(file, &mut encoded)?);
                    }
                }
                _ => {}
            }
        }
        links.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(links)
    }

    /// The attributes of the object, compact in its header first, then those in dense storage
    /// in the order of their names.
    fn attributes(&self, file: &File<'a>) -> Read<Vec<Attribute>> {
        let mut attributes = Vec::new();
        for message in &self.messages {
            match message.kind {
                ATTRIBUTE => {
                    if message.flags & SHARED != 0 {
                        return Err(unported("an attribute held in the shared message heap"));
                    }
                    attributes.push(attribute(file, &mut message.body.clone())?);
                }
                ATTRIBUTE_INFO => {
                    let mut body = message.body.clone();
                    body.u8()?;
                    let flags = body.u8()?;
                    if flags & 0x01 != 0 {
                        body.skip(2)?;
                    }
                    let heap = body.uint(file.offsets)?;
                    let index = body.uint(file.offsets)?;
                    if file.undefined(heap) || file.undefined(index) {
                        continue;
                    }
                    let heap = FractalHeap::read(file, heap)?;
                    let mut dense = Vec::new();
                    for record in btree::records(file, index)? {
                        let (Some(id), Some(&flags)) = (record.get(..8), record.get(8)) else {
                            return Err(malformed(format!(
                                "an attribute name record of {} bytes",
                                record.len()
                            )));
                        };
                        if flags & SHARED != 0 {
                            return Err(unported("an attribute held in the shared message heap"));
                        }
                        let id = &id[..heap.heap_id_length().min(id.len())];
                        let mut encoded = Cursor::new(
                            heap.object(file, id)?,
                            0,
                            Order::Little,
                            "an attribute in dense storage",
                        );
                        dense.push(attribute(file, &mut encoded)?);
                    }
                    dense.sort_by(|left, right| left.name.cmp(&right.name));
                    attributes.extend(dense);
                }
                _ => {}
            }
        }
        Ok(attributes)
    }

    /// The dataset the object is, as a variable at `path`, or none where it is no dataset, such
    /// as a named datatype.
    fn variable(&self, file: &File<'a>, path: String) -> Read<Option<Variable>> {
        if self.find(LAYOUT).is_none() {
            return Ok(None);
        }
        let space = self.find(DATASPACE).ok_or_else(|| {
            malformed(format!("the dataset at {} has no dataspace", self.address))
        })?;
        let dataspace = Dataspace::read(&mut space.body.clone(), file.lengths)?;
        let datatype = self.datatype(file)?;
        let (data_type, inner) = datatype.cdm();
        let mut axes = dataspace.axes();
        axes.extend(inner);
        Ok(Some(Variable {
            data_type,
            name: path,
            axes,
            attributes: self.attributes(file)?,
        }))
    }

    fn datatype(&self, file: &File<'a>) -> Read<Datatype> {
        let message = self
            .find(DATATYPE)
            .ok_or_else(|| malformed(format!("the dataset at {} has no datatype", self.address)))?;
        if message.flags & SHARED != 0 {
            return committed(file, &mut message.body.clone());
        }
        Datatype::read(&mut message.body.clone())
    }
}

/// The messages of a version 1 object header and its continuations.
fn version_1<'a>(file: &File<'a>, address: u64) -> Read<Vec<Message<'a>>> {
    let mut header = file.cursor(address, 16, "an object header")?;
    let version = header.u8()?;
    if version != 1 {
        return Err(header.malformed(format!("an object header of unknown version {version}")));
    }
    header.skip(7)?;
    let size = u64::from(header.u32()?);
    let mut blocks = vec![(address + 16, size)];
    let mut visited = BTreeSet::new();
    let mut messages = Vec::new();
    while let Some((start, length)) = blocks.pop() {
        if !visited.insert(start) {
            return Err(malformed(format!(
                "an object header at {address} that loops back"
            )));
        }
        let mut block = file.cursor(start, length, "an object header's messages")?;
        while block.remaining() >= 8 {
            let kind = block.u16()?;
            let size = usize::from(block.u16()?);
            let flags = block.u8()?;
            block.skip(3)?;
            let body = block.take(size)?;
            collect(file, kind, flags, body, start, &mut messages, &mut blocks)?;
        }
    }
    Ok(messages)
}

/// The messages of a version 2 object header and its continuations.
fn version_2<'a>(file: &File<'a>, address: u64) -> Read<Vec<Message<'a>>> {
    let mut header = file.cursor_from(address, "an object header")?;
    header.skip(4)?;
    let version = header.u8()?;
    if version != 2 {
        return Err(header.malformed(format!("an object header of unknown version {version}")));
    }
    let flags = header.u8()?;
    if flags & 0x20 != 0 {
        header.skip(16)?;
    }
    if flags & 0x10 != 0 {
        header.skip(4)?;
    }
    let size = header.uint(1 << (flags & 0x03))?;
    let start = address + header.position() as u64;
    let ordered = flags & 0x04 != 0;
    let mut blocks = vec![(start, size)];
    let mut visited = BTreeSet::new();
    let mut messages = Vec::new();
    while let Some((start, length)) = blocks.pop() {
        if !visited.insert(start) {
            return Err(malformed(format!(
                "an object header at {address} that loops back"
            )));
        }
        let mut block = file.cursor(start, length, "an object header's messages")?;
        let prefix = if ordered { 6 } else { 4 };
        while block.remaining() >= prefix {
            let kind = u16::from(block.u8()?);
            let size = usize::from(block.u16()?);
            let flags = block.u8()?;
            if ordered {
                block.skip(2)?;
            }
            let body = block.take(size)?;
            let before = blocks.len();
            collect(file, kind, flags, body, start, &mut messages, &mut blocks)?;
            for continued in &mut blocks[before..] {
                let (at, length) = *continued;
                let signature = file.slice(at, 4, "an object header continuation")?;
                if signature != b"OCHK" || length < 8 {
                    return Err(malformed(format!(
                        "no continuation signature at {at} of the object header at {address}"
                    )));
                }
                *continued = (at + 4, length - 8);
            }
        }
    }
    Ok(messages)
}

/// Keeps a message, or the block a continuation message points to.
fn collect<'a>(
    file: &File<'a>,
    kind: u16,
    flags: u8,
    body: &'a [u8],
    origin: u64,
    messages: &mut Vec<Message<'a>>,
    blocks: &mut Vec<(u64, u64)>,
) -> Read<()> {
    if kind == CONTINUATION {
        let mut continuation = Cursor::new(body, origin, Order::Little, "a continuation message");
        let at = continuation.uint(file.offsets)?;
        let length = continuation.uint(file.lengths)?;
        blocks.push((at, length));
        return Ok(());
    }
    messages.push(Message {
        kind,
        flags,
        body: Cursor::new(body, origin, Order::Little, "an object header message"),
    });
    Ok(())
}

/// A link message's hard link, by its name and the object header it links to, or none for a
/// soft or external link.
fn link(file: &File, body: &mut Cursor) -> Read<Option<(String, u64)>> {
    body.u8()?;
    let flags = body.u8()?;
    let kind = if flags & 0x08 != 0 { body.u8()? } else { 0 };
    if flags & 0x04 != 0 {
        body.skip(8)?;
    }
    if flags & 0x10 != 0 {
        body.u8()?;
    }
    let length = body.size(1 << (flags & 0x03), "a link name's length")?;
    let name = String::from_utf8_lossy(body.take(length)?).into_owned();
    if kind != 0 {
        return Ok(None);
    }
    let header = body.uint(file.offsets)?;
    Ok((!file.undefined(header)).then_some((name, header)))
}

/// An attribute message: its name, and its values as its datatype and dataspace decode them.
fn attribute(file: &File, body: &mut Cursor) -> Read<Attribute> {
    let version = body.u8()?;
    let flags = body.u8()?;
    let name_size = usize::from(body.u16()?);
    let datatype_size = usize::from(body.u16()?);
    let dataspace_size = usize::from(body.u16()?);
    if version == 3 {
        body.u8()?;
    }
    let padded = |size: usize| {
        if version == 1 {
            size.next_multiple_of(8)
        } else {
            size
        }
    };
    let name = crate::formats::bytes::text_to_nul(body.take(name_size)?);
    body.skip(padded(name_size) - name_size)?;
    let mut type_bytes = Cursor::new(
        body.take(padded(datatype_size))?,
        0,
        Order::Little,
        "an attribute's datatype",
    );
    let datatype = if flags & 0x01 != 0 {
        committed(file, &mut type_bytes)?
    } else {
        Datatype::read(&mut type_bytes)?
    };
    if flags & 0x02 != 0 {
        return Err(unported(format!("attribute {name} of a shared dataspace")));
    }
    let mut space_bytes = Cursor::new(
        body.take(padded(dataspace_size))?,
        0,
        Order::Little,
        "an attribute's dataspace",
    );
    let dataspace = Dataspace::read(&mut space_bytes, file.lengths)?;
    let count = dataspace
        .elements()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| {
            body.malformed(format!("attribute {name} of more values than a file holds"))
        })?;
    let values = values(&datatype, body, count, file.offsets, &|reference| {
        heap::global_object(file, reference)
    })?;
    Ok(Attribute { name, values })
}

/// The datatype of a named datatype a shared message points to.
fn committed(file: &File, shared: &mut Cursor) -> Read<Datatype> {
    let version = shared.u8()?;
    let kind = shared.u8()?;
    if version == 1 {
        shared.skip(6)?;
    } else if version >= 3 && kind == 1 {
        return Err(unported("a datatype held in the shared message heap"));
    }
    let address = shared.uint(file.offsets)?;
    let object = Object::read(file, address)?;
    let message = object.find(DATATYPE).ok_or_else(|| {
        malformed(format!(
            "the named datatype at {address} has no datatype message"
        ))
    })?;
    if message.flags & SHARED != 0 {
        return Err(malformed(format!(
            "the named datatype at {address} is itself shared"
        )));
    }
    Datatype::read(&mut message.body.clone())
}

#[cfg(test)]
mod tests;
