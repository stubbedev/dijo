//! The operators the interpreter carries out.

use super::*;

impl Machine<'_, '_> {
    /// Carries out `op`, called `depth` procedures deep.
    pub(super) fn operate(&mut self, op: Op, depth: usize) -> Run {
        match op {
            Op::Pop => {
                self.pop();
            }
            Op::Exch => {
                let b = self.pop();
                let a = self.pop();
                self.push(b)?;
                self.push(a)?;
            }
            Op::Dup => self.push(self.top())?,
            Op::Copy => self.copy()?,
            Op::Index => {
                let value = self
                    .pop_integer()
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|n| self.operands.len().checked_sub(n + 1))
                    .map_or(Value::Unknown, |at| self.operands[at]);
                self.push(value)?;
            }
            Op::Roll => {
                let shift = self.pop_integer();
                let count = self.pop_integer().and_then(|n| usize::try_from(n).ok());
                if let (Some(shift), Some(count)) = (shift, count) {
                    if count <= self.operands.len() && count > 0 {
                        let start = self.operands.len() - count;
                        let by = shift.rem_euclid(count as i64) as usize;
                        self.operands[start..].rotate_right(by);
                    }
                }
            }
            Op::Clear => self.operands.clear(),
            Op::Count => self.push(Value::Number(self.operands.len() as f64))?,
            Op::Mark => self.push(Value::Mark)?,
            Op::CountToMark => {
                let count = self.operands.iter().rev().position(|&v| v == Value::Mark);
                self.push_number(count.map(|count| count as f64))?;
            }
            Op::ClearToMark => {
                let at = self.operands.iter().rposition(|&v| v == Value::Mark);
                self.operands.truncate(at.unwrap_or(0));
            }
            Op::ArrayEnd => {
                let values = self.pop_to_mark();
                let array = self.alloc_array(values)?;
                self.push(Value::Array(array, Access::Literal))?;
            }
            Op::DictEnd => {
                let values = self.pop_to_mark();
                let mut dict = Dict::default();
                for &[key, value] in values.as_chunks::<2>().0 {
                    if let Some(key) = self.key_of(key)? {
                        dict.put(key, value);
                    }
                }
                let dict = self.alloc_dict(dict)?;
                self.push(Value::Dict(dict))?;
            }
            Op::Add => self.arithmetic(|a, b| Some(a + b))?,
            Op::Sub => self.arithmetic(|a, b| Some(a - b))?,
            Op::Mul => self.arithmetic(|a, b| Some(a * b))?,
            Op::Div => self.arithmetic(|a, b| (b != 0.0).then(|| a / b))?,
            Op::Idiv => self.integral(|a, b| a.checked_div(b))?,
            Op::Mod => self.integral(|a, b| a.checked_rem(b))?,
            Op::Neg => self.unary(|a| -a)?,
            Op::Abs => self.unary(f64::abs)?,
            Op::Ceiling => self.unary(f64::ceil)?,
            Op::Floor => self.unary(f64::floor)?,
            Op::Round => self.unary(|a| (a + 0.5).floor())?,
            Op::Truncate => self.unary(f64::trunc)?,
            Op::Sqrt => self.unary(f64::sqrt)?,
            Op::Cvi | Op::Cvr => {
                let value = self.pop();
                let converted = match value {
                    Value::String(id) => std::str::from_utf8(self.string(id))
                        .ok()
                        .and_then(|text| text.trim().parse::<f64>().ok()),
                    other => number(other),
                };
                let converted = match op {
                    Op::Cvi => converted.map(f64::trunc),
                    _ => converted,
                };
                self.push_number(converted)?;
            }
            Op::Eq | Op::Ne => {
                let b = self.pop();
                let a = self.pop();
                let equal = self.equal(a, b);
                let result = equal.map(|equal| equal == (op == Op::Eq));
                self.push(result.map_or(Value::Unknown, Value::Bool))?;
            }
            Op::Gt | Op::Ge | Op::Lt | Op::Le => {
                let b = self.pop();
                let a = self.pop();
                let order = match (a, b) {
                    (Value::Number(a), Value::Number(b)) => a.partial_cmp(&b),
                    (Value::String(a), Value::String(b)) => {
                        Some(self.string(a).cmp(self.string(b)))
                    }
                    _ => None,
                };
                let result = order.map(|order| match op {
                    Op::Gt => order.is_gt(),
                    Op::Ge => order.is_ge(),
                    Op::Lt => order.is_lt(),
                    _ => order.is_le(),
                });
                self.push(result.map_or(Value::Unknown, Value::Bool))?;
            }
            Op::And | Op::Or | Op::Xor => {
                let b = self.pop();
                let a = self.pop();
                let result = match (a, b) {
                    (Value::Bool(a), Value::Bool(b)) => Value::Bool(match op {
                        Op::And => a && b,
                        Op::Or => a || b,
                        _ => a != b,
                    }),
                    _ => match (integer(a), integer(b)) {
                        (Some(a), Some(b)) => Value::Number(match op {
                            Op::And => a & b,
                            Op::Or => a | b,
                            _ => a ^ b,
                        } as f64),
                        _ => Value::Unknown,
                    },
                };
                self.push(result)?;
            }
            Op::Not => {
                let value = self.pop();
                let result = match value {
                    Value::Bool(value) => Value::Bool(!value),
                    other => integer(other).map_or(Value::Unknown, |n| Value::Number(!n as f64)),
                };
                self.push(result)?;
            }
            Op::True => self.push(Value::Bool(true))?,
            Op::False => self.push(Value::Bool(false))?,
            Op::Null => self.push(Value::Null)?,
            Op::Type => {
                let value = self.pop();
                let name: &[u8] = match value {
                    Value::Number(n) if n.fract() == 0.0 => b"integertype",
                    Value::Number(_) => b"realtype",
                    Value::Bool(_) => b"booleantype",
                    Value::Name(..) => b"nametype",
                    Value::String(_) => b"stringtype",
                    Value::Array(..) => b"arraytype",
                    Value::Dict(_) => b"dicttype",
                    Value::Operator(_) => b"operatortype",
                    Value::Mark => b"marktype",
                    Value::Null => b"nulltype",
                    Value::File | Value::Filter(_) => b"filetype",
                    Value::Save(_) => b"savetype",
                    Value::Unknown => return self.push(Value::Unknown),
                };
                let name = self.names.intern(name);
                self.push(Value::Name(name, Access::Literal))?;
            }
            Op::Cvn => {
                let value = self.pop();
                let name = match value {
                    Value::String(id) => {
                        let text = self.string(id).clone();
                        Value::Name(self.name(&text)?, Access::Literal)
                    }
                    other @ Value::Name(..) => other,
                    _ => Value::Unknown,
                };
                self.push(name)?;
            }
            Op::Cvx | Op::Cvlit => {
                let access = if op == Op::Cvx {
                    Access::Executable
                } else {
                    Access::Literal
                };
                let value = match self.pop() {
                    Value::Name(name, _) => Value::Name(name, access),
                    Value::Array(array, _) => Value::Array(array, access),
                    other => other,
                };
                self.push(value)?;
            }
            Op::Xcheck => {
                let executable = matches!(
                    self.pop(),
                    Value::Name(_, Access::Executable)
                        | Value::Array(_, Access::Executable)
                        | Value::Operator(_)
                );
                self.push(Value::Bool(executable))?;
            }
            Op::Cvs => {
                let buffer = self.pop();
                let value = self.pop();
                let text = match value {
                    Value::Number(n) => number_text(n),
                    Value::Name(name, _) => self.names.text(name).to_vec(),
                    Value::String(id) => self.string(id).clone(),
                    Value::Bool(true) => b"true".to_vec(),
                    Value::Bool(false) => b"false".to_vec(),
                    Value::Operator(op) => op.name().to_vec(),
                    _ => NO_STRING_VALUE.to_vec(),
                };
                let Value::String(buffer) = buffer else {
                    return self.push(Value::Unknown);
                };
                let target = &mut self.strings[buffer.0];
                let written = text.len().min(target.len());
                target[..written].copy_from_slice(&text[..written]);
                let result = self.alloc_string(text[..written].to_vec())?;
                self.push(Value::String(result))?;
            }
            Op::Length => {
                let value = self.pop();
                let length = match value {
                    Value::String(id) => Some(self.string(id).len()),
                    Value::Array(id, _) => Some(self.array(id).len()),
                    Value::Dict(id) => Some(self.dict(id).len()),
                    Value::Name(name, _) => Some(self.names.text(name).len()),
                    _ => None,
                };
                self.push_number(length.map(|length| length as f64))?;
            }
            Op::Get => {
                let index = self.pop();
                let container = self.pop();
                let value = match container {
                    Value::Array(id, _) => length(index)
                        .and_then(|at| self.array(id).get(at).copied())
                        .unwrap_or(Value::Unknown),
                    Value::String(id) => length(index)
                        .and_then(|at| self.string(id).get(at))
                        .map_or(Value::Unknown, |&byte| Value::Number(f64::from(byte))),
                    Value::Dict(id) => match self.key_of(index)? {
                        Some(key) => self.dict(id).get(key).unwrap_or(Value::Unknown),
                        None => Value::Unknown,
                    },
                    _ => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::Put => {
                let value = self.pop();
                let index = self.pop();
                let container = self.pop();
                match container {
                    Value::Array(id, _) => {
                        if let Some(slot) =
                            length(index).and_then(|at| self.arrays[id.0].get_mut(at))
                        {
                            *slot = value;
                        }
                    }
                    Value::String(id) => {
                        let byte = integer(value).map(|byte| (byte & 0xFF) as u8);
                        let slot = length(index).and_then(|at| self.strings[id.0].get_mut(at));
                        if let (Some(slot), Some(byte)) = (slot, byte) {
                            *slot = byte;
                        }
                    }
                    Value::Dict(id) => {
                        if let Some(key) = self.key_of(index)? {
                            self.define(id, key, value)?;
                        }
                    }
                    _ => {}
                }
            }
            Op::GetInterval => {
                let count = self.pop();
                let index = self.pop();
                let container = self.pop();
                let range = length(index).zip(length(count)).and_then(|(at, count)| {
                    let end = at.checked_add(count)?;
                    Some(at..end)
                });
                let value = match (container, range) {
                    (Value::Array(id, access), Some(range)) => {
                        match self.array(id).get(range).map(<[Value]>::to_vec) {
                            Some(values) => Value::Array(self.alloc_array(values)?, access),
                            None => Value::Unknown,
                        }
                    }
                    (Value::String(id), Some(range)) => {
                        match self.string(id).get(range).map(<[u8]>::to_vec) {
                            Some(bytes) => Value::String(self.alloc_string(bytes)?),
                            None => Value::Unknown,
                        }
                    }
                    _ => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::PutInterval => {
                let source = self.pop();
                let index = length(self.pop());
                let target = self.pop();
                match (target, source, index) {
                    (Value::Array(target, _), Value::Array(source, _), Some(at)) => {
                        let values = self.array(source).clone();
                        let target = &mut self.arrays[target.0];
                        if let Some(slots) = at
                            .checked_add(values.len())
                            .and_then(|end| target.get_mut(at..end))
                        {
                            slots.copy_from_slice(&values);
                        }
                    }
                    (Value::String(target), Value::String(source), Some(at)) => {
                        let bytes = self.string(source).clone();
                        let target = &mut self.strings[target.0];
                        if let Some(slots) = at
                            .checked_add(bytes.len())
                            .and_then(|end| target.get_mut(at..end))
                        {
                            slots.copy_from_slice(&bytes);
                        }
                    }
                    _ => {}
                }
            }
            Op::Aload => {
                let array = self.pop();
                if let Value::Array(id, _) = array {
                    for at in 0..self.array(id).len() {
                        self.push(self.array(id)[at])?;
                    }
                }
                self.push(array)?;
            }
            Op::Astore => {
                let array = self.pop();
                if let Value::Array(id, _) = array {
                    let count = self.array(id).len();
                    let start = self.operands.len().saturating_sub(count);
                    let values = self.operands.split_off(start);
                    let target = &mut self.arrays[id.0];
                    target[count - values.len()..].copy_from_slice(&values);
                }
                self.push(array)?;
            }
            Op::PackedArray => {
                let count = self.pop_integer().and_then(|n| usize::try_from(n).ok());
                let value = match count.filter(|&count| count <= self.operands.len()) {
                    Some(count) => {
                        let values = self.operands.split_off(self.operands.len() - count);
                        Value::Array(self.alloc_array(values)?, Access::Literal)
                    }
                    None => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::Array => {
                let value = match self.pop_length() {
                    Some(count) => {
                        Value::Array(self.alloc_array(vec![Value::Null; count])?, Access::Literal)
                    }
                    None => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::String => {
                let value = match self.pop_length() {
                    Some(count) => Value::String(self.alloc_string(vec![0; count])?),
                    None => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::Dict => {
                self.pop();
                let dict = self.alloc_dict(Dict::default())?;
                self.push(Value::Dict(dict))?;
            }
            Op::Begin => {
                if self.begun.len() >= MAX_DICTIONARIES {
                    return Err(Interrupt::Stop);
                }
                let dict = match self.pop() {
                    Value::Dict(dict) => dict,
                    _ => self.alloc_dict(Dict::default())?,
                };
                self.begun.push(dict);
            }
            Op::End => {
                self.begun.pop();
            }
            Op::Def => {
                let value = self.pop();
                let key = self.pop();
                if let Some(key) = self.key_of(key)? {
                    self.define(self.current_dict(), key, value)?;
                }
            }
            Op::Load => {
                let key = self.pop();
                let value = match key {
                    Value::Name(name, _) => self
                        .lookup(name)
                        .or_else(|| self.names.operator(name).map(Value::Operator))
                        .unwrap_or(Value::Unknown),
                    _ => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::Store => {
                let value = self.pop();
                let key = self.pop();
                if let Some(key) = self.key_of(key)? {
                    let dict = self.defining(key).unwrap_or(self.current_dict());
                    self.define(dict, key, value)?;
                }
            }
            Op::Known => {
                let key = self.pop();
                let dict = self.pop();
                let known = match (dict, self.key_of(key)?) {
                    (Value::Dict(dict), Some(key)) => Some(
                        self.dict(dict).get(key).is_some()
                            || (dict == self.system && self.is_operator(key)),
                    ),
                    _ => None,
                };
                self.push(known.map_or(Value::Unknown, Value::Bool))?;
            }
            Op::Where => {
                let key = self.pop();
                let Some(key) = self.key_of(key)? else {
                    return self.push(Value::Bool(false));
                };
                let found = self
                    .defining(key)
                    .or_else(|| self.is_operator(key).then_some(self.system));
                if let Some(dict) = found {
                    self.push(Value::Dict(dict))?;
                }
                self.push(Value::Bool(found.is_some()))?;
            }
            Op::Undef => {
                let key = self.pop();
                let dict = self.pop();
                if let (Value::Dict(dict), Some(key)) = (dict, self.key_of(key)?) {
                    self.undefine(dict, key);
                }
            }
            Op::CurrentDict => self.push(Value::Dict(self.current_dict()))?,
            Op::Exec => {
                let value = self.pop();
                self.execute_found(value, depth)?;
            }
            Op::If => {
                let proc = self.pop();
                if self.pop() == Value::Bool(true) {
                    self.execute_found(proc, depth)?;
                }
            }
            Op::IfElse => {
                let otherwise = self.pop();
                let then = self.pop();
                match self.pop() {
                    Value::Bool(true) => self.execute_found(then, depth)?,
                    Value::Bool(false) => self.execute_found(otherwise, depth)?,
                    _ => {}
                }
            }
            Op::For => {
                let body = self.pop();
                let limit = self.pop_number();
                let step = self.pop_number();
                let start = self.pop_number();
                let (Some(limit), Some(step), Some(mut next)) = (limit, step, start) else {
                    return Ok(());
                };
                self.repeat(body, depth, |machine| {
                    let more = if step >= 0.0 {
                        next <= limit
                    } else {
                        next >= limit
                    };
                    if more {
                        machine.push(Value::Number(next))?;
                        next += step;
                    }
                    Ok(more)
                })?;
            }
            Op::Repeat => {
                let body = self.pop();
                let Some(mut left) = self.pop_integer() else {
                    return Ok(());
                };
                self.repeat(body, depth, |_| {
                    left -= 1;
                    Ok(left >= 0)
                })?;
            }
            Op::Loop => {
                let body = self.pop();
                self.repeat(body, depth, |_| Ok(true))?;
            }
            Op::ForAll => {
                let body = self.pop();
                let container = self.pop();
                let mut at = 0usize;
                match container {
                    Value::Array(id, _) => self.repeat(body, depth, |machine| {
                        let Some(&value) = machine.array(id).get(at) else {
                            return Ok(false);
                        };
                        at += 1;
                        machine.push(value)?;
                        Ok(true)
                    })?,
                    Value::String(id) => self.repeat(body, depth, |machine| {
                        let Some(&byte) = machine.string(id).get(at) else {
                            return Ok(false);
                        };
                        at += 1;
                        machine.push(Value::Number(f64::from(byte)))?;
                        Ok(true)
                    })?,
                    Value::Dict(id) => {
                        let entries: Vec<(Key, Value)> = self.dict(id).entries().collect();
                        self.charge(entries.len())?;
                        self.repeat(body, depth, |machine| {
                            let Some(&(key, value)) = entries.get(at) else {
                                return Ok(false);
                            };
                            at += 1;
                            let key = machine.key_value(key);
                            machine.push(key)?;
                            machine.push(value)?;
                            Ok(true)
                        })?
                    }
                    _ => {}
                }
            }
            Op::Exit => return Err(Interrupt::Exit),
            Op::Stop => return Err(Interrupt::Stop),
            Op::Stopped => {
                let body = self.pop();
                let stopped = match self.execute_found(body, depth) {
                    Ok(()) => false,
                    Err(Interrupt::Stop | Interrupt::Exit) => true,
                    Err(other) => return Err(other),
                };
                self.push(Value::Bool(stopped))?;
            }
            Op::Quit => return Err(Interrupt::Quit),
            Op::Inert => {}
            Op::Save => {
                if self.saves.len() >= MAX_SAVED_STATES {
                    return Err(Interrupt::Stop);
                }
                self.saves.push(Saved {
                    journal: self.journal.len(),
                    graphics: self.graphics_state(),
                    stacked: self.graphics.len(),
                });
                self.push(Value::Save(self.saves.len() - 1))?;
            }
            Op::Restore => {
                if let Value::Save(level) = self.pop() {
                    self.restore(level);
                }
            }
            Op::GSave => {
                if self.graphics.len() >= MAX_SAVED_STATES {
                    return Err(Interrupt::Stop);
                }
                self.graphics.push(self.graphics_state());
            }
            Op::GRestore => {
                let floor = self.saves.last().map_or(0, |saved| saved.stacked);
                if self.graphics.len() > floor {
                    if let Some(graphics) = self.graphics.pop() {
                        self.set_graphics_state(graphics);
                    }
                }
            }
            Op::GRestoreAll => {
                let floor = self.saves.last().map_or(0, |saved| saved.stacked);
                if let Some(&graphics) = self.graphics.get(floor) {
                    self.set_graphics_state(graphics);
                }
                self.graphics.truncate(floor);
            }
            Op::CurrentFile => self.push(Value::File)?,
            Op::Eexec => self.eexec()?,
            Op::ReadString | Op::ReadHexString | Op::ReadLine => self.read(op)?,
            Op::Filter => {
                let filter = self.pop();
                let mut source = self.pop();
                if matches!(source, Value::Dict(_)) {
                    source = self.pop();
                }
                let terminator = match filter {
                    Value::Name(name, _) => match self.names.text(name) {
                        b"ASCII85Decode" => Some(Terminator::Base85),
                        b"ASCIIHexDecode" => Some(Terminator::Hex),
                        _ => None,
                    },
                    _ => None,
                };
                let value = match source {
                    Value::File => Value::Filter(terminator),
                    Value::Filter(inner) => Value::Filter(inner),
                    _ => Value::Unknown,
                };
                self.push(value)?;
            }
            Op::FindFont => {
                let key = self.pop();
                let font = self.find_font(key)?;
                self.push(font)?;
            }
            Op::ScaleFont => {
                let scale = self.pop_number();
                let font = self.pop();
                let font = match scale {
                    Some(scale) => self.transform_font(font, [scale, 0.0, 0.0, scale, 0.0, 0.0])?,
                    None => Value::Unknown,
                };
                self.push(font)?;
            }
            Op::MakeFont => {
                let matrix = self.pop();
                let font = self.pop();
                let font = match self.matrix(matrix) {
                    Some(matrix) => self.transform_font(font, matrix)?,
                    None => Value::Unknown,
                };
                self.push(font)?;
            }
            Op::SetFont => {
                self.font = match self.pop() {
                    Value::Dict(font) => Some(font),
                    _ => None,
                };
            }
            Op::SelectFont => {
                let size = self.pop();
                let key = self.pop();
                let font = self.find_font(key)?;
                let matrix = match size {
                    Value::Number(scale) => Some([scale, 0.0, 0.0, scale, 0.0, 0.0]),
                    other => self.matrix(other),
                };
                let font = match matrix {
                    Some(matrix) => self.transform_font(font, matrix)?,
                    None => Value::Unknown,
                };
                self.font = match font {
                    Value::Dict(font) => Some(font),
                    _ => None,
                };
            }
            Op::DefineFont => {
                let font = self.pop();
                let key = self.pop();
                let defined = self.define_font(key, font)?;
                self.push(defined)?;
            }
            Op::UndefineFont => {
                let key = self.pop();
                if let Some(key) = self.key_of(key)? {
                    self.undefine(self.font_directory, key);
                }
            }
            Op::CurrentFont => {
                let font = self.font.map_or(Value::Unknown, Value::Dict);
                self.push(font)?;
            }
            Op::FindResource => {
                let category = self.pop();
                let key = self.pop();
                let found = match category {
                    Value::Name(category, _) if category == self.known.font => {
                        self.find_font(key)?
                    }
                    Value::Name(category, _) => match self.key_of(key)? {
                        Some(key) => self
                            .resources
                            .get(&(category, key))
                            .copied()
                            .unwrap_or(Value::Unknown),
                        None => Value::Unknown,
                    },
                    _ => Value::Unknown,
                };
                self.push(found)?;
            }
            Op::DefineResource => {
                let category = self.pop();
                let instance = self.pop();
                let key = self.pop();
                let defined = match category {
                    Value::Name(category, _) if category == self.known.font => {
                        self.define_font(key, instance)?
                    }
                    Value::Name(category, _) => {
                        if let Some(key) = self.key_of(key)? {
                            self.charge(1)?;
                            self.resources.insert((category, key), instance);
                        }
                        instance
                    }
                    _ => Value::Unknown,
                };
                self.push(defined)?;
            }
            Op::ResourceStatus => {
                let category = self.pop();
                let key = self.pop();
                let found = match (category, self.key_of(key)?) {
                    (Value::Name(category, _), Some(key)) if category == self.known.font => {
                        self.dict(self.font_directory).get(key).is_some()
                    }
                    (Value::Name(category, _), Some(key)) => {
                        self.resources.contains_key(&(category, key))
                    }
                    _ => false,
                };
                if found {
                    self.push(Value::Number(0.0))?;
                    self.push(Value::Number(0.0))?;
                }
                self.push(Value::Bool(found))?;
            }
            Op::UndefineResource => {
                let category = self.pop();
                let key = self.pop();
                if let (Value::Name(category, _), Some(key)) = (category, self.key_of(key)?) {
                    self.resources.remove(&(category, key));
                }
            }
            Op::MoveTo | Op::LineTo => {
                let point = self.pop_point();
                self.layout.move_to(point);
            }
            Op::RMoveTo | Op::RLineTo => {
                let offset = self.pop_point();
                self.layout.move_by(offset);
            }
            Op::CurveTo => {
                let end = self.pop_point();
                self.drop_operands(4);
                self.layout.move_to(end);
            }
            Op::RCurveTo => {
                let end = self.pop_point();
                self.drop_operands(4);
                self.layout.move_by(end);
            }
            Op::Arc | Op::ArcN => {
                let end = self.pop_number();
                self.pop();
                let radius = self.pop_number();
                let centre = self.pop_point();
                let point = match (centre, radius, end) {
                    (Some((x, y)), Some(radius), Some(end)) => {
                        let (sin, cos) = end.to_radians().sin_cos();
                        Some((x + radius * cos, y + radius * sin))
                    }
                    _ => None,
                };
                self.layout.move_to(point);
            }
            Op::ArcT => {
                self.drop_operands(5);
                self.layout.clear_point();
            }
            Op::ClosePath | Op::NewPath | Op::Paint => self.layout.clear_point(),
            Op::CurrentPoint => match self.layout.position().current() {
                Some((x, y)) => {
                    self.push(Value::Number(x))?;
                    self.push(Value::Number(y))?;
                }
                None => {
                    self.push(Value::Unknown)?;
                    self.push(Value::Unknown)?;
                }
            },
            Op::Show | Op::KShow | Op::CShow => {
                let string = self.pop();
                if op != Op::Show {
                    self.pop();
                }
                self.show(string, Spacing::None)?;
            }
            Op::AShow => {
                let string = self.pop();
                self.pop();
                let letter = self.pop_number();
                self.show(string, letter.map_or(Spacing::None, Spacing::Letters))?;
            }
            Op::WidthShow => {
                let string = self.pop();
                let code = self.pop_integer();
                self.pop();
                let extra = self.pop_number();
                let spacing = match (code, extra) {
                    (Some(code), Some(extra)) => Spacing::Words {
                        code: (code & 0xFF) as u8,
                        extra,
                    },
                    _ => Spacing::None,
                };
                self.show(string, spacing)?;
            }
            Op::AWidthShow => {
                let string = self.pop();
                self.pop();
                let letter = self.pop_number();
                let code = self.pop_integer();
                self.pop();
                let word = self.pop_number();
                let spacing = match (code, word, letter) {
                    (Some(code), Some(word), Some(letter)) => Spacing::Both {
                        code: (code & 0xFF) as u8,
                        word,
                        letter,
                    },
                    _ => Spacing::None,
                };
                self.show(string, spacing)?;
            }
            Op::XShow | Op::YShow | Op::XYShow => {
                let displacements = self.pop();
                let string = self.pop();
                let width = match displacements {
                    Value::Array(id, _) => {
                        let numbers: Option<Vec<f64>> =
                            self.array(id).iter().map(|&value| number(value)).collect();
                        numbers.map(|numbers| match op {
                            Op::XShow => numbers.iter().sum(),
                            Op::XYShow => numbers.iter().step_by(2).sum(),
                            _ => 0.0,
                        })
                    }
                    _ => None,
                };
                self.show(string, Spacing::Displaced(width))?;
            }
            Op::GlyphShow => {
                let glyph = self.pop();
                self.glyph_show(glyph)?;
            }
            Op::StringWidth => {
                let string = self.pop();
                let width = match string {
                    Value::String(id) => self
                        .decode(&self.string(id).clone())
                        .ok()
                        .and_then(|decoded| decoded.width),
                    _ => None,
                };
                self.push_number(width)?;
                self.push_number(width.map(|_| 0.0))?;
            }
            Op::CharPath => self.drop_operands(2),
            Op::ShowPage => self.show_page()?,
            Op::Image => self.image(Sampled::Image, depth)?,
            Op::ImageMask => self.image(Sampled::Mask, depth)?,
            Op::ColorImage => self.image(Sampled::Color, depth)?,
            Op::Matrix => {
                let matrix = self.alloc_array(IDENTITY.map(Value::Number).to_vec())?;
                self.push(Value::Array(matrix, Access::Literal))?;
            }
            Op::Translate | Op::Scale => {
                let matrix = self.pop_matrix_operand();
                self.drop_operands(2);
                if let Some(matrix) = matrix {
                    self.push(matrix)?;
                }
            }
            Op::Rotate => {
                let matrix = self.pop_matrix_operand();
                self.pop();
                if let Some(matrix) = matrix {
                    self.push(matrix)?;
                }
            }
            Op::ConcatMatrix => {
                let target = self.pop();
                self.drop_operands(2);
                self.push(target)?;
            }
            Op::Transform => {
                self.pop_matrix_operand();
                self.drop_operands(2);
                self.push(Value::Unknown)?;
                self.push(Value::Unknown)?;
            }
            Op::InvertMatrix => {
                let target = self.pop();
                self.pop();
                self.push(target)?;
            }
            Op::Rect => {
                let operands = if matches!(self.top(), Value::Number(_)) {
                    4
                } else {
                    1
                };
                self.drop_operands(operands);
                self.layout.clear_point();
            }
            Op::Consume(count) => self.drop_operands(usize::from(count)),
            Op::Inquire(count) => {
                for _ in 0..count {
                    self.push(Value::Unknown)?;
                }
            }
            Op::LanguageLevel => self.push(Value::Number(3.0))?,
            Op::CurrentGlobal => self.push(Value::Bool(false))?,
            Op::Version => {
                let version = self.alloc_string(b"3010".to_vec())?;
                self.push(Value::String(version))?;
            }
        }
        Ok(())
    }

    fn drop_operands(&mut self, count: usize) {
        let keep = self.operands.len().saturating_sub(count);
        self.operands.truncate(keep);
    }

    fn pop_length(&mut self) -> Option<usize> {
        self.pop_integer()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n <= MAX_OBJECT_LENGTH)
    }

    /// The matrix a transforming operator takes last, if it is given one.
    fn pop_matrix_operand(&mut self) -> Option<Value> {
        matches!(self.top(), Value::Array(..)).then(|| self.pop())
    }

    fn pop_to_mark(&mut self) -> Vec<Value> {
        match self.operands.iter().rposition(|&v| v == Value::Mark) {
            Some(at) => {
                let values = self.operands.split_off(at + 1);
                self.operands.pop();
                values
            }
            None => std::mem::take(&mut self.operands),
        }
    }

    fn arithmetic(&mut self, apply: impl Fn(f64, f64) -> Option<f64>) -> Run {
        let b = self.pop_number();
        let a = self.pop_number();
        self.push_number(a.zip(b).and_then(|(a, b)| apply(a, b)))
    }

    fn integral(&mut self, apply: impl Fn(i64, i64) -> Option<i64>) -> Run {
        let b = self.pop_integer();
        let a = self.pop_integer();
        let result = a.zip(b).and_then(|(a, b)| apply(a, b));
        self.push_number(result.map(|n| n as f64))
    }

    fn unary(&mut self, apply: impl Fn(f64) -> f64) -> Run {
        let a = self.pop_number();
        self.push_number(a.map(apply))
    }

    fn is_operator(&self, key: Key) -> bool {
        matches!(key, Key::Name(name) if self.names.operator(name).is_some())
    }

    /// Whether two values are equal as `eq` compares them, `None` where either is unknown.
    fn equal(&self, a: Value, b: Value) -> Option<bool> {
        let text = |value: Value| match value {
            Value::String(id) => Some(self.string(id).as_slice()),
            Value::Name(name, _) => Some(self.names.text(name)),
            _ => None,
        };
        Some(match (a, b) {
            (Value::Unknown, _) | (_, Value::Unknown) => return None,
            (Value::Name(a, _), Value::Name(b, _)) => a == b,
            (Value::Array(a, _), Value::Array(b, _)) => a == b,
            _ => match (text(a), text(b)) {
                (Some(a), Some(b)) => a == b,
                _ => a == b,
            },
        })
    }

    /// Copies operands, or one composite object into another.
    fn copy(&mut self) -> Run {
        let top = self.pop();
        match (self.top(), top) {
            (Value::Array(source, _), Value::Array(target, access)) => {
                self.pop();
                let values = self.array(source).clone();
                let slots = &mut self.arrays[target.0];
                let count = values.len().min(slots.len());
                slots[..count].copy_from_slice(&values[..count]);
                self.push(Value::Array(target, access))
            }
            (Value::String(source), Value::String(target)) => {
                self.pop();
                let bytes = self.string(source).clone();
                let slots = &mut self.strings[target.0];
                let count = bytes.len().min(slots.len());
                slots[..count].copy_from_slice(&bytes[..count]);
                self.push(Value::String(target))
            }
            (Value::Dict(source), Value::Dict(target)) => {
                self.pop();
                let entries: Vec<(Key, Value)> = self.dict(source).entries().collect();
                for (key, value) in entries {
                    self.define(target, key, value)?;
                }
                self.push(Value::Dict(target))
            }
            (_, count) => {
                let Some(count) = length(count).filter(|&n| n <= self.operands.len()) else {
                    return Ok(());
                };
                let start = self.operands.len() - count;
                for at in start..start + count {
                    self.push(self.operands[at])?;
                }
                Ok(())
            }
        }
    }
}
