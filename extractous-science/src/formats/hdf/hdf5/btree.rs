//! HDF5's B-trees, read in place: the version 1 B-tree of a group's symbol table, and the
//! version 2 B-tree that indexes a group's links and an object's attributes in dense storage.

use std::collections::BTreeSet;

use super::heap::encoded_size;
use super::File;
use crate::formats::bytes::{malformed, Read};

/// A link of a group's symbol table: its name's offset in the local heap, and the object header
/// it links to, where it is a hard link.
pub(super) struct Symbol {
    pub(super) name_offset: u64,
    pub(super) header: Option<u64>,
}

/// The symbols of the group whose symbol table B-tree is at `root`, in the tree's order.
pub(super) fn symbols(file: &File, root: u64) -> Read<Vec<Symbol>> {
    let mut symbols = Vec::new();
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !visited.insert(node) {
            return Err(malformed(format!(
                "a group B-tree that loops back to node {node}"
            )));
        }
        let mut cursor = file.cursor_from(node, "a group B-tree node")?;
        if cursor.take(4)? != b"TREE" {
            return Err(cursor.malformed("no B-tree signature"));
        }
        let node_type = cursor.u8()?;
        if node_type != 0 {
            return Err(cursor.malformed(format!("a B-tree of type {node_type} for a group")));
        }
        let level = cursor.u8()?;
        let entries = usize::from(cursor.u16()?);
        cursor.skip(2 * file.offsets)?;
        let mut children = Vec::with_capacity(entries);
        for _ in 0..entries {
            cursor.skip(file.lengths)?;
            children.push(cursor.uint(file.offsets)?);
        }
        if level > 0 {
            pending.extend(children.into_iter().rev());
            continue;
        }
        for child in children {
            symbol_node(file, child, &mut symbols)?;
        }
    }
    Ok(symbols)
}

/// The symbols of a symbol table node.
fn symbol_node(file: &File, address: u64, symbols: &mut Vec<Symbol>) -> Read<()> {
    let mut cursor = file.cursor_from(address, "a symbol table node")?;
    if cursor.take(4)? != b"SNOD" {
        return Err(cursor.malformed("no symbol table node signature"));
    }
    cursor.skip(2)?;
    let count = cursor.u16()?;
    for _ in 0..count {
        let name_offset = cursor.uint(file.offsets)?;
        let header = cursor.uint(file.offsets)?;
        let cache = cursor.u32()?;
        cursor.skip(20)?;
        symbols.push(Symbol {
            name_offset,
            header: (cache != 2 && !file.undefined(header)).then_some(header),
        });
    }
    Ok(())
}

/// What a version 2 B-tree's header says of its nodes.
struct Tree {
    node_size: usize,
    record_size: usize,
    /// The bytes of a child's count of records.
    count_size: usize,
    /// For each depth, the most records a node holds and the bytes of a subtree's total count
    /// of records.
    levels: Vec<(usize, usize)>,
}

/// Every record of the version 2 B-tree whose header is at `address`, each `record_size` bytes
/// as stored.
pub(super) fn records<'a>(file: &File<'a>, address: u64) -> Read<Vec<&'a [u8]>> {
    let mut header = file.cursor_from(address, "a version 2 B-tree header")?;
    if header.take(4)? != b"BTHD" {
        return Err(header.malformed("no version 2 B-tree signature"));
    }
    header.skip(2)?;
    let node_size = header.size(4, "a node's size")?;
    let record_size = usize::from(header.u16()?);
    let depth = usize::from(header.u16()?);
    header.skip(2)?;
    let root = header.uint(file.offsets)?;
    let root_records = usize::from(header.u16()?);
    if record_size == 0 || node_size <= 10 {
        return Err(header.malformed(format!(
            "nodes of {node_size} bytes with records of {record_size}"
        )));
    }
    let leaf_records = (node_size - 10) / record_size;
    let count_size = encoded_size(leaf_records as u64);
    let mut levels = vec![(leaf_records, 0usize)];
    let mut cumulative = leaf_records as u64;
    for level in 1..=depth {
        let pointer = file.offsets + count_size + if level > 1 { levels[level - 1].1 } else { 0 };
        let records = (node_size.saturating_sub(10 + pointer)) / (record_size + pointer);
        cumulative = (records as u64 + 1)
            .saturating_mul(cumulative)
            .saturating_add(records as u64);
        levels.push((records, encoded_size(cumulative)));
    }
    let tree = Tree {
        node_size,
        record_size,
        count_size,
        levels,
    };
    let mut found = Vec::new();
    if file.undefined(root) || root_records == 0 {
        return Ok(found);
    }
    let mut pending = vec![(root, root_records, depth)];
    let mut visited = BTreeSet::new();
    while let Some((node, count, depth)) = pending.pop() {
        if !visited.insert(node) {
            return Err(malformed(format!(
                "a version 2 B-tree that loops back to node {node}"
            )));
        }
        tree.node(file, node, count, depth, &mut found, &mut pending)?;
    }
    Ok(found)
}

impl Tree {
    fn node<'a>(
        &self,
        file: &File<'a>,
        address: u64,
        count: usize,
        depth: usize,
        found: &mut Vec<&'a [u8]>,
        pending: &mut Vec<(u64, usize, usize)>,
    ) -> Read<()> {
        let mut cursor = file.cursor(address, self.node_size as u64, "a version 2 B-tree node")?;
        let signature = if depth == 0 { b"BTLF" } else { b"BTIN" };
        if cursor.take(4)? != signature {
            return Err(cursor.malformed("no version 2 B-tree node signature"));
        }
        cursor.skip(2)?;
        if count > self.levels[depth].0 {
            return Err(cursor.malformed(format!(
                "{count} records where a node holds {}",
                self.levels[depth].0
            )));
        }
        for _ in 0..count {
            found.push(cursor.take(self.record_size)?);
        }
        if depth == 0 {
            return Ok(());
        }
        for _ in 0..=count {
            let child = cursor.uint(file.offsets)?;
            let records = cursor.size(self.count_size, "a child's count of records")?;
            if depth > 1 {
                cursor.skip(self.levels[depth - 1].1)?;
            }
            pending.push((child, records, depth - 1));
        }
        Ok(())
    }
}
