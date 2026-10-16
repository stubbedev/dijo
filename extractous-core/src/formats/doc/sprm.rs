//! POI's `SprmOperation` and `SprmIterator`: the single property modifiers of a grpprl, each an
//! opcode with an operand whose size the opcode gives, read as POI reads them, an operand past
//! the end of the grpprl failing as Java's array bounds do.

use crate::formats::poi::{i16_at, i32_at, Thrown};

/// Property kinds of an opcode, as `SprmOperation` names them.
pub(super) const TYPE_PAP: u16 = 1;
pub(super) const TYPE_CHP: u16 = 2;
pub(super) const TYPE_TAP: u16 = 5;

const SPRM_LONG_PARAGRAPH: u16 = 0xC615;
const SPRM_LONG_TABLE: u16 = 0xD608;

/// One property modifier of a grpprl.
#[derive(Debug, Clone, Copy)]
pub(super) struct SprmOperation<'a> {
    grpprl: &'a [u8],
    value: u16,
    /// Where the operand starts.
    pub(super) operand_offset: usize,
    size: usize,
}

impl<'a> SprmOperation<'a> {
    /// `new SprmOperation(grpprl, offset)`.
    pub(super) fn new(grpprl: &'a [u8], offset: usize) -> Result<Self, Thrown> {
        let value = i16_at(grpprl, offset)? as u16;
        let mut operation = SprmOperation {
            grpprl,
            value,
            operand_offset: offset + 2,
            size: 0,
        };
        operation.size = match operation.size_code() {
            0 | 1 => 3,
            2 | 4 | 5 => 4,
            3 => 6,
            6 => {
                if value == SPRM_LONG_TABLE || value == SPRM_LONG_PARAGRAPH {
                    let size = usize::from(i16_at(grpprl, operation.operand_offset)? as u16) + 3;
                    operation.operand_offset += 2;
                    size
                } else {
                    let size = usize::from(byte(grpprl, operation.operand_offset)?) + 3;
                    operation.operand_offset += 1;
                    size
                }
            }
            _ => 5,
        };
        Ok(operation)
    }

    /// The operation within its kind.
    pub(super) fn operation(&self) -> u16 {
        self.value & 0x1FF
    }

    pub(super) fn kind(&self) -> u16 {
        (self.value & 0x1C00) >> 10
    }

    pub(super) fn size_code(&self) -> u16 {
        (self.value & 0xE000) >> 13
    }

    pub(super) fn size(&self) -> usize {
        self.size
    }

    pub(super) fn grpprl(&self) -> &'a [u8] {
        self.grpprl
    }

    /// `getOperand`.
    pub(super) fn operand(&self) -> Result<i32, Thrown> {
        let at = self.operand_offset;
        Ok(match self.size_code() {
            0 | 1 => i32::from(byte(self.grpprl, at)? as i8),
            2 | 4 | 5 => i32::from(i16_at(self.grpprl, at)?),
            3 => i32_at(self.grpprl, at)?,
            6 => {
                let length = byte(self.grpprl, at + 1)? as i8;
                let mut code = [0u8; 4];
                for i in 0..length.max(0) as usize {
                    if at + i < self.grpprl.len() {
                        *code
                            .get_mut(i)
                            .ok_or_else(|| Thrown::new("an operand longer than four bytes"))? =
                            byte(self.grpprl, at + 1 + i)?;
                    }
                }
                i32::from_le_bytes(code)
            }
            _ => i32::from_le_bytes([
                byte(self.grpprl, at)?,
                byte(self.grpprl, at + 1)?,
                byte(self.grpprl, at + 2)?,
                0,
            ]),
        })
    }

    /// `getOperandShortSigned`.
    pub(super) fn operand_short(&self) -> Result<i16, Thrown> {
        if !matches!(self.size_code(), 2 | 4 | 5) {
            return Err(Thrown::new("an operand that is no signed short"));
        }
        i16_at(self.grpprl, self.operand_offset)
    }
}

fn byte(grpprl: &[u8], at: usize) -> Result<u8, Thrown> {
    grpprl
        .get(at)
        .copied()
        .ok_or_else(|| Thrown::new("a sprm runs past its grpprl"))
}

/// `SprmIterator`: the operations of a grpprl from `offset`.
pub(super) struct SprmIterator<'a> {
    grpprl: &'a [u8],
    offset: usize,
}

impl<'a> SprmIterator<'a> {
    pub(super) fn new(grpprl: &'a [u8], offset: usize) -> Self {
        SprmIterator { grpprl, offset }
    }

    /// `hasNext`.
    pub(super) fn has_next(&self) -> bool {
        (self.offset as i64) < self.grpprl.len() as i64 - 1
    }

    /// `next`.
    pub(super) fn next_operation(&mut self) -> Result<SprmOperation<'a>, Thrown> {
        let operation = SprmOperation::new(self.grpprl, self.offset)?;
        self.offset += operation.size();
        Ok(operation)
    }
}

/// `SprmBuffer.findSprm`: the first operation of a kind and operation from offset 2.
pub(super) fn find_sprm(grpprl: &[u8], opcode: u16) -> Result<Option<SprmOperation<'_>>, Thrown> {
    let mut iterator = SprmIterator::new(grpprl, 2);
    while iterator.has_next() {
        let operation = iterator.next_operation()?;
        if operation.operation() == opcode & 0x1FF && operation.kind() == (opcode & 0x1C00) >> 10 {
            return Ok(Some(operation));
        }
    }
    Ok(None)
}

/// Whether a grpprl from `offset` holds an operation of `kind`.
pub(super) fn has_kind(grpprl: &[u8], offset: usize, kind: u16) -> Result<bool, Thrown> {
    let mut iterator = SprmIterator::new(grpprl, offset);
    while iterator.has_next() {
        if iterator.next_operation()?.kind() == kind {
            return Ok(true);
        }
    }
    Ok(false)
}
