//! The formats database of 1-2-3 97 and later (`.123`), which keeps the formats of the cells apart
//! from them, run-length encoded, as Gnumeric's Lotus import reads it.
//!
//! The database lies between two `FORMATS` records. Its `PACKINFO` record gives the size of each
//! dimension, from the innermost, rows, to the outermost, sheets, each a 16-bit count, or a 32-bit
//! one in a file of BOF version 0x1005, and the size of the record tells which. A `NODE` record
//! adds a run of its count of the deepest dimension not yet filled to the run that holds it, and
//! a run of rows is a leaf, whose format a `DATANODE` record after it gives. A run is full when
//! the runs in it fill its dimension, and then the next `NODE` goes to the run that holds it. A `REGISTERID` record names the run the next `NODE` adds, and a
//! `USEREGISTEREDID` record adds that run again, whole, where the next run would go. The
//! `BEGIN_GROUP` and `END_GROUP` records around the runs carry nothing a run needs.

use std::collections::HashMap;
use std::rc::Rc;

use super::format::Style;
use super::reader::Record;

/// The record that starts and ends the formats database.
pub(super) const FORMATS: u16 = 0x0293;
pub(super) const NODE: u16 = 0x0800;
pub(super) const DATANODE: u16 = 0x0801;
pub(super) const REGISTER_ID: u16 = 0x0802;
pub(super) const USE_REGISTERED_ID: u16 = 0x0803;
pub(super) const PACKINFO: u16 = 0x0804;

/// The dimensions of the formats database: sheets, columns and rows.
const DIMENSIONS: usize = 3;

/// A run of the database: the end of its place in its dimension, past its last, and what it holds.
#[derive(Debug, Clone)]
struct Run {
    end: u32,
    node: Node,
}

/// What a run holds: the runs of the dimension within it, or, a run of rows, its format.
#[derive(Debug, Clone)]
enum Node {
    Runs(Rc<[Run]>),
    Leaf(Option<Style>),
}

impl Node {
    /// The format at `place`, by sheet, column and row, `None` where no run holds it or holds
    /// no format.
    fn style(&self, place: &[u32]) -> Option<Style> {
        match (self, place) {
            (Node::Leaf(style), []) => *style,
            (Node::Runs(runs), [at, within @ ..]) => {
                let run = runs.get(runs.partition_point(|run| run.end <= *at))?;
                run.node.style(within)
            }
            _ => None,
        }
    }
}

/// A formats database read whole: the formats of the cells by sheet, column and row.
#[derive(Debug)]
pub(super) struct Database(Node);

impl Database {
    pub(super) fn style(&self, sheet: u8, column: u8, row: u16) -> Option<Style> {
        self.0
            .style(&[u32::from(sheet), u32::from(column), u32::from(row)])
    }
}

/// A run not yet full: how much of its dimension it takes and how much of the one within it is
/// filled, the runs in it so far, and the name a `REGISTERID` gave it.
#[derive(Debug)]
struct Open {
    length: u32,
    filled: u32,
    runs: Vec<Run>,
    id: Option<u16>,
}

/// A registered run: its depth, its length and what it holds.
#[derive(Debug, Clone)]
struct Registered {
    depth: usize,
    length: u32,
    node: Node,
}

/// A formats database being read from its `PACKINFO` record on.
#[derive(Debug)]
struct Builder {
    /// The size of each dimension, from the outermost.
    sizes: [u32; DIMENSIONS],
    /// The byte size of a count: 2, or 4 in a file of BOF version 0x1005.
    count_size: usize,
    /// The runs not yet full, from the whole database to the deepest.
    open: Vec<Open>,
    registered: HashMap<u16, Registered>,
    /// The name the next run is given.
    pending_id: Option<u16>,
    /// The name of the leaf the last `NODE` added, if it has one, whose format a `DATANODE` may
    /// still give; `Some(None)` for a leaf of no name, `None` when the last record added no leaf.
    last_leaf: Option<Option<u16>>,
}

impl Builder {
    fn new(record: &Record<'_>) -> Result<Self, String> {
        let dimensions = usize::from(record.u16(2)?);
        if dimensions != DIMENSIONS {
            return Err(format!(
                "the formats database at {} has {dimensions} dimensions, not the {DIMENSIONS} of \
                 sheets, columns and rows",
                record.offset
            ));
        }
        let count_size = (record.body.len() - 4) / DIMENSIONS;
        if !matches!(count_size, 2 | 4) || 4 + count_size * DIMENSIONS != record.body.len() {
            return Err(format!(
                "the PACKINFO record at {} of {} bytes holds no counts of 2 or 4 bytes",
                record.offset,
                record.body.len()
            ));
        }
        let mut sizes = [0; DIMENSIONS];
        for (index, size) in sizes.iter_mut().rev().enumerate() {
            *size = count(record, 4 + index * count_size, count_size)?;
        }
        Ok(Builder {
            sizes,
            count_size,
            open: vec![Open {
                length: 1,
                filled: 0,
                runs: Vec::new(),
                id: None,
            }],
            registered: HashMap::new(),
            pending_id: None,
            last_leaf: None,
        })
    }

    /// Closes the runs past the outermost that are full, each into the run that holds it.
    fn settle(&mut self) {
        while self.open.len() > 1 && self.is_full(self.open.len() - 1) {
            self.close_deepest();
        }
    }

    fn is_full(&self, depth: usize) -> bool {
        self.open[depth].filled == self.sizes[depth]
    }

    fn close_deepest(&mut self) {
        let depth = self.open.len() - 1;
        let closed = self.open.pop().expect("a run past the outermost");
        let node = Node::Runs(closed.runs.into());
        if let Some(id) = closed.id {
            self.registered.insert(
                id,
                Registered {
                    depth,
                    length: closed.length,
                    node: node.clone(),
                },
            );
        }
        self.add(closed.length, node);
    }

    /// Adds a run of `length` holding `node` to the deepest open run.
    fn add(&mut self, length: u32, node: Node) {
        let parent = self.open.last_mut().expect("the outermost run");
        parent.filled += length;
        parent.runs.push(Run {
            end: parent.filled,
            node,
        });
    }

    /// The deepest open run, failing on a database already full.
    fn deepest(&mut self, record: &Record<'_>) -> Result<usize, String> {
        self.settle();
        let depth = self.open.len() - 1;
        if self.is_full(depth) {
            return Err(format!(
                "the record at {} adds a run to a formats database already full",
                record.offset
            ));
        }
        Ok(depth)
    }

    /// Checks that a run of `length` fits what is left of the dimension of the runs of `depth`.
    fn fits(&self, record: &Record<'_>, depth: usize, length: u32) -> Result<(), String> {
        let left = self.sizes[depth] - self.open[depth].filled;
        if length == 0 || length > left {
            return Err(format!(
                "the record at {} adds a run of {length} where {left} are left",
                record.offset
            ));
        }
        Ok(())
    }

    fn node(&mut self, record: &Record<'_>) -> Result<(), String> {
        let length = count(record, 0, self.count_size)?;
        let depth = self.deepest(record)?;
        self.fits(record, depth, length)?;
        let id = self.pending_id.take();
        if depth + 1 == DIMENSIONS {
            let leaf = Node::Leaf(None);
            if let Some(id) = id {
                let registered = Registered {
                    depth: DIMENSIONS,
                    length,
                    node: leaf.clone(),
                };
                self.registered.insert(id, registered);
            }
            self.add(length, leaf);
            self.last_leaf = Some(id);
            return Ok(());
        }
        self.last_leaf = None;
        self.open.push(Open {
            length,
            filled: 0,
            runs: Vec::new(),
            id,
        });
        Ok(())
    }

    fn data(&mut self, record: &Record<'_>, style: Style) -> Result<(), String> {
        let Some(id) = self.last_leaf.take() else {
            return Err(format!(
                "the DATANODE record at {} follows no run of rows",
                record.offset
            ));
        };
        let leaf = &mut self
            .open
            .last_mut()
            .and_then(|deepest| deepest.runs.last_mut())
            .expect("the run of rows the last NODE added")
            .node;
        *leaf = Node::Leaf(Some(style));
        if let Some(registered) = id.and_then(|id| self.registered.get_mut(&id)) {
            registered.node = leaf.clone();
        }
        Ok(())
    }

    fn register(&mut self, record: &Record<'_>) -> Result<(), String> {
        self.last_leaf = None;
        self.pending_id = Some(record.u16(0)?);
        Ok(())
    }

    fn use_registered(&mut self, record: &Record<'_>) -> Result<(), String> {
        self.last_leaf = None;
        let id = record.u16(0)?;
        let depth = self.deepest(record)?;
        let Some(registered) = self.registered.get(&id).cloned() else {
            return Err(format!(
                "the record at {} uses the run {id}, which no REGISTERID names",
                record.offset
            ));
        };
        if registered.depth != depth + 1 {
            return Err(format!(
                "the record at {} uses the run {id} of depth {} at depth {}",
                record.offset,
                registered.depth,
                depth + 1
            ));
        }
        self.fits(record, depth, registered.length)?;
        self.add(registered.length, registered.node);
        Ok(())
    }

    /// The database, its runs not full closed as they are.
    fn finish(mut self) -> Database {
        while self.open.len() > 1 {
            self.close_deepest();
        }
        let outermost = self.open.pop().expect("the outermost run");
        Database(Node::Runs(outermost.runs.into()))
    }
}

/// A count of `size` bytes at `at`.
fn count(record: &Record<'_>, at: usize, size: usize) -> Result<u32, String> {
    match size {
        2 => Ok(u32::from(record.u16(at)?)),
        _ => record.u32(at),
    }
}

/// What of the formats database the records read so far hold.
#[derive(Debug, Default)]
pub(super) struct Reader(State);

#[derive(Debug, Default)]
enum State {
    #[default]
    Before,
    /// Between its `FORMATS` records, from its `PACKINFO` record on.
    Within(Option<Builder>),
    After(Database),
}

impl Reader {
    /// Reads a record of the database, of a kind [`is_database`] tells.
    pub(super) fn read(&mut self, record: &Record<'_>) -> Result<(), String> {
        if record.kind == FORMATS {
            self.0 = match std::mem::take(&mut self.0) {
                State::Before => State::Within(None),
                State::Within(Some(builder)) => State::After(builder.finish()),
                State::Within(None) => {
                    return Err(format!(
                        "the formats database ending at {} has no PACKINFO record",
                        record.offset
                    ))
                }
                State::After(_) => {
                    return Err(format!(
                        "the FORMATS record at {} starts a second formats database",
                        record.offset
                    ))
                }
            };
            return Ok(());
        }
        let State::Within(builder) = &mut self.0 else {
            return Ok(());
        };
        if record.kind == PACKINFO {
            if builder.is_some() {
                return Err(format!(
                    "the formats database has a second PACKINFO record at {}",
                    record.offset
                ));
            }
            *builder = Some(Builder::new(record)?);
            return Ok(());
        }
        let Some(builder) = builder else {
            return Err(format!(
                "the record of type {:#06x} at {} comes before the PACKINFO record of its \
                 formats database",
                record.kind, record.offset
            ));
        };
        match record.kind {
            NODE => builder.node(record),
            DATANODE => builder.data(record, Style::of_database(record.bytes(0)?)),
            REGISTER_ID => builder.register(record),
            USE_REGISTERED_ID => builder.use_registered(record),
            kind => unreachable!("the record of type {kind:#06x} is of no formats database"),
        }
    }

    /// The database read, if the file has one and it has ended.
    pub(super) fn database(&self) -> Option<&Database> {
        match &self.0 {
            State::After(database) => Some(database),
            State::Before | State::Within(_) => None,
        }
    }
}

/// Whether records of `kind` are of the formats database.
pub(super) fn is_database(kind: u16) -> bool {
    kind == FORMATS || (NODE..=PACKINFO).contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::lotus_123::reader::records;

    fn record(kind: u16, body: &[u8]) -> Vec<u8> {
        let length = u16::try_from(body.len()).unwrap();
        [&kind.to_le_bytes()[..], &length.to_le_bytes(), body].concat()
    }

    fn node(length: u16) -> Vec<u8> {
        record(NODE, &length.to_le_bytes())
    }

    fn read(stream: &[Vec<u8>]) -> Result<Reader, String> {
        let bytes = stream.concat();
        let mut reader = Reader::default();
        for record in records(&bytes) {
            reader.read(&record?)?;
        }
        Ok(reader)
    }

    /// Two sheets of four columns of eight rows.
    fn packinfo() -> Vec<u8> {
        record(PACKINFO, &[3, 0, 3, 0, 8, 0, 4, 0, 2, 0])
    }

    #[test]
    fn reads_runs_by_sheet_column_and_row_reusing_registered_runs() {
        let reader = read(&[
            record(FORMATS, &[]),
            packinfo(),
            node(1),
            node(1),
            record(REGISTER_ID, &[7, 0]),
            node(8),
            node(2),
            node(2),
            record(DATANODE, &[0x02, 0x02, 0, 0]),
            node(3),
            record(DATANODE, &[0x72, 0, 0, 0]),
            node(3),
            node(1),
            record(USE_REGISTERED_ID, &[7, 0]),
            node(1),
            node(4),
            node(8),
            record(FORMATS, &[]),
        ])
        .unwrap();
        let database = reader.database().unwrap();
        assert_eq!(database.style(0, 0, 5), None);
        let parenthesised = Style {
            code: 0x02,
            parentheses: true,
        };
        assert_eq!(database.style(0, 1, 1), Some(parenthesised));
        assert_eq!(database.style(0, 2, 4), Some(Style::code(0x72)));
        assert_eq!(database.style(0, 2, 5), None);
        assert_eq!(database.style(0, 3, 0), None);
        assert_eq!(database.style(1, 0, 0), None);
        assert_eq!(database.style(2, 0, 0), None);
    }

    #[test]
    fn reads_counts_of_four_bytes() {
        let reader = read(&[
            record(FORMATS, &[]),
            record(PACKINFO, &[3, 0, 3, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0]),
            record(NODE, &1u32.to_le_bytes()),
            record(NODE, &1u32.to_le_bytes()),
            record(NODE, &65_536u32.to_le_bytes()),
            record(DATANODE, &[0x31, 0, 0, 0]),
            record(FORMATS, &[]),
        ])
        .unwrap();
        let database = reader.database().unwrap();
        assert_eq!(database.style(0, 0, 65_535), Some(Style::code(0x31)));
        assert_eq!(database.style(0, 1, 0), None);
    }

    #[test]
    fn fails_runs_past_their_dimension_and_unknown_names() {
        let error = read(&[record(FORMATS, &[]), packinfo(), node(3)]).unwrap_err();
        assert!(error.contains("a run of 3 where 2 are left"), "{error}");
        let error = read(&[
            record(FORMATS, &[]),
            packinfo(),
            node(1),
            record(USE_REGISTERED_ID, &[9, 0]),
        ])
        .unwrap_err();
        assert!(error.contains("which no REGISTERID names"), "{error}");
        let error = read(&[record(FORMATS, &[]), node(1)]).unwrap_err();
        assert!(error.contains("before the PACKINFO record"), "{error}");
    }
}
