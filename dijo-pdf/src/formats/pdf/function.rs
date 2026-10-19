//! PDFBox's `PDFunction`: the functions of PDF, sampled (type 0), exponential (type 2),
//! stitching (type 3) and PostScript calculator (type 4), evaluated in single precision as
//! PDFBox evaluates them, and the identity function `/Identity` names.

use std::rc::Rc;

use lopdf::{Object, Stream};

use super::cos::{as_dict, float_value, Pdf};
use super::Unported;

/// Why a function failed: an exception PDFBox throws while it makes or evaluates the function,
/// or data the port does not decode.
#[derive(Debug)]
pub(super) enum Failed {
    Thrown(String),
    Unported(Unported),
}

impl From<Unported> for Failed {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn thrown(message: impl Into<String>) -> Failed {
    Failed::Thrown(message.into())
}

/// A function as `PDFunction.create` makes it.
#[derive(Debug, Clone)]
pub(super) struct Function {
    kind: Kind,
    domain: Vec<f32>,
    /// `Range`, which the outputs are clipped to, if the function has one.
    range: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
enum Kind {
    Identity,
    Sampled(Rc<Sampled>),
    Exponential {
        c0: Vec<f32>,
        c1: Vec<f32>,
        exponent: f32,
    },
    Stitching {
        functions: Vec<Function>,
        bounds: Vec<f32>,
        encode: Vec<f32>,
    },
    Calculator(Rc<Procedure>),
}

/// `PDFunctionType0`'s table: its sizes, the samples of each point read once, and how they
/// map to the outputs.
#[derive(Debug)]
struct Sampled {
    size: Vec<f32>,
    bits_per_sample: i32,
    encode: Vec<f32>,
    decode: Option<Vec<f32>>,
    samples: Vec<Vec<i64>>,
}

/// How deep functions may stitch functions before the port stops following them.
const MAX_NESTING: usize = 16;

impl Function {
    /// `PDFunction.create` of `object`.
    pub(super) fn create(pdf: &Pdf, object: &Object) -> Result<Self, Failed> {
        Self::nested(pdf, object, 0)
    }

    fn nested(pdf: &Pdf, object: &Object, depth: usize) -> Result<Self, Failed> {
        if matches!(object, Object::Name(name) if name == b"Identity") {
            return Ok(Self {
                kind: Kind::Identity,
                domain: Vec::new(),
                range: None,
            });
        }
        if depth > MAX_NESTING {
            return Err(Unported(format!(
                "functions stitching functions deeper than {MAX_NESTING}, which PDFBox follows \
                 further"
            ))
            .into());
        }
        let resolved = pdf.resolve(object);
        let Some(dict) = resolved.and_then(as_dict) else {
            return Err(thrown(
                "Error: Function must be a Dictionary, but is not one",
            ));
        };
        let numbers = |key: &[u8]| -> Option<Vec<f32>> {
            pdf.array(dict, key)
                .map(|items| items.iter().map(|item| number(pdf, item)).collect())
        };
        let domain = numbers(b"Domain").ok_or_else(|| thrown("a function without a domain"))?;
        let range = numbers(b"Range");
        let stream = match resolved {
            Some(Object::Stream(stream)) => Some(stream),
            _ => None,
        };
        let kind = match pdf.int(dict, b"FunctionType", -1) {
            0 => {
                let stream = stream.ok_or_else(|| thrown("a sampled function without data"))?;
                let size =
                    numbers(b"Size").ok_or_else(|| thrown("a sampled function without a size"))?;
                Kind::Sampled(Rc::new(Sampled::read(
                    pdf,
                    stream,
                    size,
                    numbers(b"Encode"),
                    numbers(b"Decode").or_else(|| range.clone()),
                    domain.len() / 2,
                    range.as_ref().map_or(0, |range| range.len() / 2),
                )?))
            }
            2 => {
                let coefficients = |key: &[u8], default: f32| -> Result<Vec<f32>, Failed> {
                    match pdf.array(dict, key) {
                        Some(items) if !items.is_empty() => items
                            .iter()
                            .map(|item| {
                                pdf.resolve(item)
                                    .and_then(float_value)
                                    .ok_or_else(|| thrown("a coefficient that is not a number"))
                            })
                            .collect(),
                        _ => Ok(vec![default]),
                    }
                };
                Kind::Exponential {
                    c0: coefficients(b"C0", 0.0)?,
                    c1: coefficients(b"C1", 1.0)?,
                    exponent: pdf.float(dict, b"N", -1.0),
                }
            }
            3 => {
                let functions = pdf
                    .array(dict, b"Functions")
                    .ok_or_else(|| thrown("a stitching function without functions"))?
                    .iter()
                    .map(|item| Self::nested(pdf, item, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Kind::Stitching {
                    functions,
                    bounds: numbers(b"Bounds").unwrap_or_default(),
                    encode: numbers(b"Encode").unwrap_or_default(),
                }
            }
            4 => {
                let stream =
                    stream.ok_or_else(|| thrown("a calculator function without a program"))?;
                let program: String = pdf.decode(stream)?.iter().map(|&b| char::from(b)).collect();
                Kind::Calculator(Rc::new(Procedure::parse(&program)?))
            }
            other => return Err(thrown(format!("Error: Unknown function type {other}"))),
        };
        Ok(Self {
            kind,
            domain,
            range,
        })
    }

    /// `getNumberOfOutputParameters`: half the length of `Range`, 0 without one.
    pub(super) fn output_count(&self) -> usize {
        self.range.as_ref().map_or(0, |range| range.len() / 2)
    }

    /// `eval`.
    pub(super) fn eval(&self, input: &[f32]) -> Result<Vec<f32>, Failed> {
        match &self.kind {
            Kind::Identity => Ok(input.to_vec()),
            Kind::Sampled(sampled) => self.eval_sampled(sampled, input),
            Kind::Exponential { c0, c1, exponent } => {
                let x = *input.first().ok_or_else(|| thrown("no input"))?;
                let x_to_n = f64::from(x).powf(f64::from(*exponent)) as f32;
                let result: Vec<f32> = c0
                    .iter()
                    .zip(c1)
                    .map(|(&c0, &c1)| c0 + x_to_n * (c1 - c0))
                    .collect();
                self.clipped(result)
            }
            Kind::Stitching {
                functions,
                bounds,
                encode,
            } => self.eval_stitching(functions, bounds, encode, input),
            Kind::Calculator(procedure) => self.eval_calculator(procedure, input),
        }
    }

    /// `getDomainForInput(n)`.
    fn domain_of(&self, n: usize) -> Result<(f32, f32), Failed> {
        pair(&self.domain, n)
    }

    /// `clipToRange(float[])`: each output clipped to its range, as many as the ranges.
    fn clipped(&self, values: Vec<f32>) -> Result<Vec<f32>, Failed> {
        let Some(range) = self.range.as_ref().filter(|range| !range.is_empty()) else {
            return Ok(values);
        };
        (0..range.len() / 2)
            .map(|i| {
                let value = *values
                    .get(i)
                    .ok_or_else(|| thrown("fewer outputs than the range has"))?;
                Ok(clip(value, range[i * 2], range[i * 2 + 1]))
            })
            .collect()
    }

    fn eval_sampled(&self, sampled: &Sampled, input: &[f32]) -> Result<Vec<f32>, Failed> {
        let max_sample = (2f64.powi(sampled.bits_per_sample) - 1.0) as f32;
        let mut input = input.to_vec();
        let mut previous = vec![0i32; input.len()];
        let mut next = vec![0i32; input.len()];
        for (i, value) in input.iter_mut().enumerate() {
            let (min, max) = self.domain_of(i)?;
            if sampled.encode.len() < i * 2 + 1 {
                return Err(thrown("no encoding for an input"));
            }
            let (encode_min, encode_max) = pair(&sampled.encode, i)?;
            let size = *sampled
                .size
                .get(i)
                .ok_or_else(|| thrown("no size for an input"))?;
            *value = clip(*value, min, max);
            *value = interpolate(*value, min, max, encode_min, encode_max);
            *value = clip(*value, 0.0, size - 1.0);
            previous[i] = value.floor() as i32;
            next[i] = value.ceil() as i32;
        }
        if input.is_empty() {
            return Err(thrown("a sampled function evaluated without inputs"));
        }
        let outputs = self.output_count();
        let mut coordinates = vec![0i32; input.len()];
        let mut values =
            sampled.interpolated(&input, &previous, &next, &mut coordinates, 0, outputs)?;
        for (i, value) in values.iter_mut().enumerate() {
            let range = self.range.as_deref().unwrap_or_default();
            let (range_min, range_max) = pair(range, i)?;
            let decode = sampled
                .decode
                .as_deref()
                .filter(|decode| decode.len() > i * 2)
                .ok_or_else(|| thrown("Range missing in function /Decode entry"))?;
            let (decode_min, decode_max) = pair(decode, i)?;
            *value = interpolate(*value, 0.0, max_sample, decode_min, decode_max);
            *value = clip(*value, range_min, range_max);
        }
        Ok(values)
    }

    fn eval_stitching(
        &self,
        functions: &[Function],
        bounds: &[f32],
        encode: &[f32],
        input: &[f32],
    ) -> Result<Vec<f32>, Failed> {
        let (min, max) = self.domain_of(0)?;
        let mut x = clip(*input.first().ok_or_else(|| thrown("no input"))?, min, max);
        let mut chosen = None;
        if functions.len() == 1 {
            let (encode_min, encode_max) = pair(encode, 0)?;
            x = interpolate(x, min, max, encode_min, encode_max);
            chosen = functions.first();
        } else {
            let mut partitions = Vec::with_capacity(bounds.len() + 2);
            partitions.push(min);
            partitions.extend_from_slice(bounds);
            partitions.push(max);
            let last = partitions.len() - 2;
            for i in 0..partitions.len() - 1 {
                let upper = partitions[i + 1];
                let within = x < upper || (i == last && x.total_cmp(&upper).is_eq());
                if x >= partitions[i] && within {
                    let function = functions
                        .get(i)
                        .ok_or_else(|| thrown("fewer functions than partitions"))?;
                    let (encode_min, encode_max) = pair(encode, i)?;
                    x = interpolate(x, partitions[i], upper, encode_min, encode_max);
                    chosen = Some(function);
                    break;
                }
            }
        }
        let function = chosen.ok_or_else(|| thrown("partition not found in type 3 function"))?;
        let values = function.eval(&[x])?;
        self.clipped(values)
    }

    fn eval_calculator(&self, procedure: &Procedure, input: &[f32]) -> Result<Vec<f32>, Failed> {
        let mut stack = Vec::with_capacity(16);
        for (i, &value) in input.iter().enumerate() {
            let (min, max) = self.domain_of(i)?;
            stack.push(Value::Real(clip(value, min, max)));
        }
        procedure.run(&mut stack)?;
        let outputs = self.output_count();
        if stack.len() < outputs {
            return Err(thrown(format!(
                "The type 4 function returned {} values but the Range entry indicates that \
                 {outputs} values be returned.",
                stack.len()
            )));
        }
        let mut values = vec![0f32; outputs];
        let range = self.range.as_deref().unwrap_or_default();
        for i in (0..outputs).rev() {
            let (min, max) = pair(range, i)?;
            values[i] = clip(pop_real(&mut stack)?, min, max);
        }
        Ok(values)
    }
}

impl Sampled {
    /// The table of a sampled function of `inputs` inputs and `outputs` outputs: each point's
    /// samples read bit by bit as `getSamples` reads them, those past the end of the data 0.
    fn read(
        pdf: &Pdf,
        stream: &Stream,
        size: Vec<f32>,
        encode: Option<Vec<f32>>,
        decode: Option<Vec<f32>>,
        inputs: usize,
        outputs: usize,
    ) -> Result<Self, Failed> {
        let bits_per_sample = pdf.int(&stream.dict, b"BitsPerSample", -1);
        let encode = encode.unwrap_or_else(|| {
            size.iter()
                .flat_map(|&size| [0.0, (size as i32 as i64 - 1) as f32])
                .collect()
        });
        let mut points = 1usize;
        for i in 0..inputs {
            let size = *size.get(i).ok_or_else(|| thrown("no size for an input"))?;
            points = points
                .checked_mul(usize::try_from(size as i32).map_err(|_| thrown("a negative size"))?)
                .ok_or_else(|| thrown("a sampled function too large to hold"))?;
        }
        if points.saturating_mul(outputs) > MAX_SAMPLES {
            return Err(Unported(format!(
                "a sampled function of {points} points, more than the port holds"
            ))
            .into());
        }
        let mut samples = vec![vec![0i64; outputs]; points];
        if !(1..=32).contains(&bits_per_sample) {
            return Ok(Self {
                size,
                bits_per_sample,
                encode,
                decode,
                samples,
            });
        }
        let data = pdf.decode(stream)?;
        let mut at = 0usize;
        let total_bits = data.len() * 8;
        'points: for point in samples.iter_mut() {
            for sample in point.iter_mut() {
                if at + bits_per_sample as usize > total_bits {
                    break 'points;
                }
                let mut value = 0i64;
                for _ in 0..bits_per_sample {
                    let bit = (data[at / 8] >> (7 - at % 8)) & 1;
                    value = (value << 1) | i64::from(bit);
                    at += 1;
                }
                *sample = i64::from(value as i32);
            }
        }
        Ok(Self {
            size,
            bits_per_sample,
            encode,
            decode,
            samples,
        })
    }

    /// `Rinterpol.rinterpol`: the outputs interpolated between the points around `input`, one
    /// dimension after the other, from `step` on.
    fn interpolated(
        &self,
        input: &[f32],
        previous: &[i32],
        next: &[i32],
        coordinates: &mut [i32],
        step: usize,
        outputs: usize,
    ) -> Result<Vec<f32>, Failed> {
        let last = step + 1 == input.len();
        let at = |coordinates: &mut [i32], this: &Self| -> Result<Vec<f32>, Failed> {
            match last {
                true => {
                    let point = this.point(coordinates)?;
                    Ok((0..outputs)
                        .map(|i| point.get(i).copied().unwrap_or(0) as f32)
                        .collect())
                }
                false => this.interpolated(input, previous, next, coordinates, step + 1, outputs),
            }
        };
        if previous[step] == next[step] {
            coordinates[step] = previous[step];
            return at(coordinates, self);
        }
        coordinates[step] = previous[step];
        let first = at(coordinates, self)?;
        coordinates[step] = next[step];
        let second = at(coordinates, self)?;
        Ok(first
            .iter()
            .zip(&second)
            .map(|(&first, &second)| {
                interpolate(
                    input[step],
                    previous[step] as f32,
                    next[step] as f32,
                    first,
                    second,
                )
            })
            .collect())
    }

    /// The samples of the point at `coordinates`, by `calcSampleIndex`.
    fn point(&self, coordinates: &[i32]) -> Result<&[i64], Failed> {
        let dimensions = coordinates.len();
        let size = |i: usize| {
            self.size
                .get(i)
                .copied()
                .ok_or_else(|| thrown(format!("Index {i} out of bounds")))
        };
        let mut product: i32 = 1;
        for i in (0..dimensions.saturating_sub(1)).rev() {
            product = (product as f32 * size(i)?) as i32;
        }
        let mut index: i32 = 0;
        for i in (0..dimensions).rev() {
            index = index.wrapping_add(product.wrapping_mul(coordinates[i]));
            if i >= 1 {
                product = (product as f32 / size(i - 1)?) as i32;
            }
        }
        usize::try_from(index)
            .ok()
            .and_then(|index| self.samples.get(index))
            .map(Vec::as_slice)
            .ok_or_else(|| thrown(format!("sample index {index} out of bounds")))
    }
}

/// The most samples a sampled function's table may hold.
const MAX_SAMPLES: usize = 1 << 24;

/// `PDRange(array, n)`: the `n`th pair of numbers.
fn pair(values: &[f32], n: usize) -> Result<(f32, f32), Failed> {
    match (values.get(n * 2), values.get(n * 2 + 1)) {
        (Some(&min), Some(&max)) => Ok((min, max)),
        _ => Err(thrown(format!("Index {} out of bounds", n * 2 + 1))),
    }
}

/// `COSArray.toFloatArray`'s reading of an item: a number's value, anything else 0.
fn number(pdf: &Pdf, item: &Object) -> f32 {
    pdf.resolve(item).and_then(float_value).unwrap_or(0.0)
}

/// `clipToRange(x, min, max)`.
fn clip(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// `PDFunction.interpolate`.
pub(super) fn interpolate(x: f32, x_min: f32, x_max: f32, y_min: f32, y_max: f32) -> f32 {
    if x_max == x_min {
        return y_min;
    }
    y_min + ((x - x_min) * (y_max - y_min) / (x_max - x_min))
}

/// A value of a calculator function's stack.
#[derive(Debug, Clone)]
enum Value {
    Int(i32),
    Real(f32),
    Bool(bool),
    Procedure(Rc<Procedure>),
}

impl Value {
    /// `Number.floatValue`.
    fn real(&self) -> Result<f32, Failed> {
        match self {
            Self::Int(value) => Ok(*value as f32),
            Self::Real(value) => Ok(*value),
            _ => Err(thrown("java.lang.ClassCastException: not a number")),
        }
    }

    /// `Number.doubleValue`.
    fn double(&self) -> Result<f64, Failed> {
        match self {
            Self::Int(value) => Ok(f64::from(*value)),
            Self::Real(value) => Ok(f64::from(*value)),
            _ => Err(thrown("java.lang.ClassCastException: not a number")),
        }
    }

    /// `Number.intValue`.
    fn int(&self) -> Result<i32, Failed> {
        match self {
            Self::Int(value) => Ok(*value),
            Self::Real(value) => Ok(*value as i32),
            _ => Err(thrown("java.lang.ClassCastException: not a number")),
        }
    }
}

/// An instruction of a calculator function: a value to push, or an operator to run.
#[derive(Debug)]
enum Instruction {
    Push(Value),
    Operator(String),
}

/// `InstructionSequence`.
#[derive(Debug, Default)]
struct Procedure(Vec<Instruction>);

/// How deep procedures may run procedures before the port stops following them.
const MAX_PROCEDURE_DEPTH: usize = 64;

impl Procedure {
    /// `InstructionSequenceBuilder.parse` of a program: `{` and `}` open and close
    /// procedures, integers and reals are pushed, anything else names an operator.
    fn parse(program: &str) -> Result<Self, Failed> {
        let mut open: Vec<Vec<Instruction>> = vec![Vec::new()];
        let mut closed_main = None;
        for token in tokens(program) {
            if closed_main.is_some() {
                return Err(thrown("java.util.EmptyStackException"));
            }
            match token {
                "{" => open.push(Vec::new()),
                "}" => {
                    let sequence = open.pop().expect("a sequence is open");
                    match open.last_mut() {
                        Some(parent) => parent.push(Instruction::Push(Value::Procedure(Rc::new(
                            Procedure(sequence),
                        )))),
                        None => closed_main = Some(sequence),
                    }
                }
                _ => {
                    let instruction = if is_integer(token) {
                        Instruction::Push(Value::Int(token.parse::<i32>().map_err(|_| {
                            thrown(format!("java.lang.NumberFormatException: {token}"))
                        })?))
                    } else if is_real(token) {
                        Instruction::Push(Value::Real(parse_real(token)?))
                    } else {
                        Instruction::Operator(token.to_owned())
                    };
                    open.last_mut()
                        .expect("a sequence is open")
                        .push(instruction);
                }
            }
        }
        if let Some(main) = closed_main {
            return Ok(Procedure(main));
        }
        while open.len() > 1 {
            let procedure = Procedure(open.pop().expect("a procedure is open"));
            open.last_mut()
                .expect("the main sequence stays")
                .push(Instruction::Push(Value::Procedure(Rc::new(procedure))));
        }
        Ok(Procedure(open.pop().expect("the main sequence stays")))
    }

    /// `InstructionSequence.execute`, then each procedure left on top of the stack.
    fn run(&self, stack: &mut Vec<Value>) -> Result<(), Failed> {
        self.execute(stack, 0)
    }

    fn execute(&self, stack: &mut Vec<Value>, depth: usize) -> Result<(), Failed> {
        if depth > MAX_PROCEDURE_DEPTH {
            return Err(thrown("java.lang.StackOverflowError"));
        }
        for instruction in &self.0 {
            match instruction {
                Instruction::Push(value) => stack.push(value.clone()),
                Instruction::Operator(name) => operate(name, stack, depth)?,
            }
        }
        while let Some(Value::Procedure(procedure)) = stack.last() {
            let procedure = Rc::clone(procedure);
            stack.pop();
            procedure.execute(stack, depth + 1)?;
        }
        Ok(())
    }
}

/// The tokens of `Parser.Tokenizer`: braces alone, other runs between its whitespace,
/// comments left out.
fn tokens(program: &str) -> Vec<&str> {
    let is_space = |c: char| matches!(c, '\0' | '\t' | ' ' | '\r' | '\n' | '\u{c}');
    let mut tokens = Vec::new();
    let mut rest = program;
    while let Some(c) = rest.chars().next() {
        if is_space(c) {
            rest = &rest[c.len_utf8()..];
        } else if c == '%' {
            let end = rest.find(['\r', '\n', '\u{c}']).unwrap_or(rest.len());
            rest = &rest[end..];
        } else if c == '{' || c == '}' {
            tokens.push(&rest[..1]);
            rest = &rest[1..];
        } else {
            let end = rest
                .find(|c: char| is_space(c) || c == '{' || c == '}' || c == '\u{4}')
                .unwrap_or(rest.len());
            tokens.push(&rest[..end]);
            rest = &rest[end..];
            if rest.starts_with('\u{4}') {
                rest = &rest[1..];
            }
        }
    }
    tokens
}

/// `INTEGER_PATTERN`: `[+-]?\d+`.
fn is_integer(token: &str) -> bool {
    let digits = token.strip_prefix(['+', '-']).unwrap_or(token);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `REAL_PATTERN`: `-?\d*\.\d*([Ee]-?\d+)?`.
fn is_real(token: &str) -> bool {
    let rest = token.strip_prefix('-').unwrap_or(token);
    let mantissa_end = rest.find(['E', 'e']).unwrap_or(rest.len());
    let (mantissa, exponent) = rest.split_at(mantissa_end);
    let Some((whole, fraction)) = mantissa.split_once('.') else {
        return false;
    };
    let digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return false;
    }
    match exponent.get(1..) {
        None => true,
        Some(exponent) => {
            let exponent = exponent.strip_prefix('-').unwrap_or(exponent);
            !exponent.is_empty() && digits(exponent)
        }
    }
}

/// `Float.parseFloat` of a token `REAL_PATTERN` matches, which fails on one without digits.
fn parse_real(token: &str) -> Result<f32, Failed> {
    let unsigned = token.strip_prefix('-').unwrap_or(token);
    if !unsigned.starts_with(|c: char| c.is_ascii_digit())
        && !unsigned[1..].starts_with(|c: char| c.is_ascii_digit())
    {
        return Err(thrown(format!(
            "java.lang.NumberFormatException: For input string: \"{token}\""
        )));
    }
    token
        .parse::<f32>()
        .map_err(|_| thrown(format!("java.lang.NumberFormatException: {token}")))
}

fn pop(stack: &mut Vec<Value>) -> Result<Value, Failed> {
    stack
        .pop()
        .ok_or_else(|| thrown("java.util.EmptyStackException"))
}

/// `ExecutionContext.popReal`.
fn pop_real(stack: &mut Vec<Value>) -> Result<f32, Failed> {
    pop(stack)?.real()
}

/// `ExecutionContext.popInt`: an `Integer`, which a cast to it insists on.
fn pop_int(stack: &mut Vec<Value>) -> Result<i32, Failed> {
    match pop(stack)? {
        Value::Int(value) => Ok(value),
        _ => Err(thrown("java.lang.ClassCastException: not an Integer")),
    }
}

/// `Math.toRadians`.
fn to_radians(degrees: f32) -> f64 {
    f64::from(degrees) * 0.017_453_292_519_943_295
}

/// An integer result of two integers, a real where it leaves the range of an `int`.
fn int_or_real(value: i64) -> Value {
    match i32::try_from(value) {
        Ok(value) => Value::Int(value),
        Err(_) => Value::Real(value as f32),
    }
}

/// Runs the operator `name` of PDFBox's `Operators` on `stack`.
fn operate(name: &str, stack: &mut Vec<Value>, depth: usize) -> Result<(), Failed> {
    let value = match name {
        "abs" => match pop(stack)? {
            Value::Int(value) => Value::Int(value.wrapping_abs()),
            other => Value::Real(other.real()?.abs()),
        },
        "add" | "sub" | "mul" => {
            let second = pop(stack)?;
            let first = pop(stack)?;
            match (&first, &second, name) {
                (Value::Int(a), Value::Int(b), "add") => int_or_real(i64::from(*a) + i64::from(*b)),
                (Value::Int(a), Value::Int(b), "sub") => int_or_real(i64::from(*a) - i64::from(*b)),
                (Value::Int(a), Value::Int(b), _) => int_or_real(i64::from(*a) * i64::from(*b)),
                (_, _, "add") => Value::Real(first.real()? + second.real()?),
                (_, _, "sub") => Value::Real(first.real()? - second.real()?),
                _ => Value::Real((first.double()? * second.double()?) as f32),
            }
        }
        "atan" => {
            let denominator = pop_real(stack)?;
            let numerator = pop_real(stack)?;
            let atan = f64::from(numerator).atan2(f64::from(denominator)) as f32;
            let mut degrees = (f64::from(atan).to_degrees() as f32) % 360.0;
            if degrees < 0.0 {
                degrees += 360.0;
            }
            Value::Real(degrees)
        }
        "ceiling" | "floor" | "round" | "truncate" => match pop(stack)? {
            Value::Int(value) => Value::Int(value),
            other => Value::Real(match name {
                "ceiling" => other.double()?.ceil() as f32,
                "floor" => other.double()?.floor() as f32,
                "round" => java_round(other.double()?) as f32,
                _ => other.real()? as i32 as f32,
            }),
        },
        "cos" => Value::Real(to_radians(pop_real(stack)?).cos() as f32),
        "sin" => Value::Real(to_radians(pop_real(stack)?).sin() as f32),
        "cvi" => Value::Int(pop(stack)?.int()?),
        "cvr" => Value::Real(pop(stack)?.real()?),
        "div" => {
            let second = pop(stack)?.real()?;
            let first = pop(stack)?.real()?;
            Value::Real(first / second)
        }
        "exp" => {
            let exponent = pop(stack)?.double()?;
            let base = pop(stack)?.double()?;
            Value::Real(base.powf(exponent) as f32)
        }
        "idiv" | "mod" => {
            let second = pop_int(stack)?;
            let first = pop_int(stack)?;
            if second == 0 {
                return Err(thrown("java.lang.ArithmeticException: / by zero"));
            }
            Value::Int(match name {
                "idiv" => first.wrapping_div(second),
                _ => first.wrapping_rem(second),
            })
        }
        "ln" => Value::Real(pop(stack)?.double()?.ln() as f32),
        "log" => Value::Real(pop(stack)?.double()?.log10() as f32),
        "neg" => match pop(stack)? {
            Value::Int(i32::MIN) => Value::Real(-(i32::MIN as f32)),
            Value::Int(value) => Value::Int(-value),
            other => Value::Real(-other.real()?),
        },
        "sqrt" => {
            let value = pop_real(stack)?;
            if value < 0.0 {
                return Err(thrown(
                    "java.lang.IllegalArgumentException: argument must be nonnegative",
                ));
            }
            Value::Real(f64::from(value).sqrt() as f32)
        }
        "and" | "or" | "xor" => {
            let second = pop(stack)?;
            let first = pop(stack)?;
            match (first, second) {
                (Value::Bool(a), Value::Bool(b)) => Value::Bool(match name {
                    "and" => a && b,
                    "or" => a || b,
                    _ => a ^ b,
                }),
                (Value::Int(a), Value::Int(b)) => Value::Int(match name {
                    "and" => a & b,
                    "or" => a | b,
                    _ => a ^ b,
                }),
                _ => {
                    return Err(thrown(
                        "java.lang.ClassCastException: Operands must be bool/bool or int/int",
                    ))
                }
            }
        }
        "bitshift" => {
            let shift = pop_int(stack)?;
            let value = pop_int(stack)?;
            Value::Int(match shift < 0 {
                true => value.wrapping_shr(shift.wrapping_abs() as u32),
                false => value.wrapping_shl(shift as u32),
            })
        }
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "not" => match pop(stack)? {
            Value::Bool(value) => Value::Bool(!value),
            Value::Int(value) => Value::Int(value.wrapping_neg()),
            _ => {
                return Err(thrown(
                    "java.lang.ClassCastException: Operand must be bool or int",
                ))
            }
        },
        "eq" | "ne" => {
            let second = pop(stack)?;
            let first = pop(stack)?;
            let equal = match (&first, &second) {
                (Value::Int(_) | Value::Real(_), Value::Int(_) | Value::Real(_)) => {
                    first.real()?.total_cmp(&second.real()?).is_eq()
                }
                (Value::Bool(a), Value::Bool(b)) => a == b,
                (Value::Procedure(a), Value::Procedure(b)) => Rc::ptr_eq(a, b),
                _ => false,
            };
            Value::Bool(equal == (name == "eq"))
        }
        "ge" | "gt" | "le" | "lt" => {
            let second = pop(stack)?.real()?;
            let first = pop(stack)?.real()?;
            Value::Bool(match name {
                "ge" => first >= second,
                "gt" => first > second,
                "le" => first <= second,
                _ => first < second,
            })
        }
        "if" | "ifelse" => {
            let otherwise = match name {
                "ifelse" => Some(procedure(pop(stack)?)?),
                _ => None,
            };
            let then = procedure(pop(stack)?)?;
            let condition = match pop(stack)? {
                Value::Bool(condition) => condition,
                _ => return Err(thrown("java.lang.ClassCastException: not a Boolean")),
            };
            match (condition, otherwise) {
                (true, _) => then.execute(stack, depth + 1)?,
                (false, Some(otherwise)) => otherwise.execute(stack, depth + 1)?,
                (false, None) => {}
            }
            return Ok(());
        }
        "copy" => {
            let count = pop(stack)?.int()?;
            if count > 0 {
                let count = count as usize;
                let start = stack
                    .len()
                    .checked_sub(count)
                    .ok_or_else(|| thrown("java.lang.IndexOutOfBoundsException"))?;
                let copied: Vec<Value> = stack[start..].to_vec();
                stack.extend(copied);
            }
            return Ok(());
        }
        "dup" => stack
            .last()
            .cloned()
            .ok_or_else(|| thrown("java.util.EmptyStackException"))?,
        "exch" => {
            let second = pop(stack)?;
            let first = pop(stack)?;
            stack.push(second);
            first
        }
        "index" => {
            let n = pop(stack)?.int()?;
            if n < 0 {
                return Err(thrown(format!(
                    "java.lang.IllegalArgumentException: rangecheck: {n}"
                )));
            }
            let at = stack
                .len()
                .checked_sub(n as usize + 1)
                .ok_or_else(|| thrown("java.lang.ArrayIndexOutOfBoundsException"))?;
            stack[at].clone()
        }
        "pop" => {
            pop(stack)?;
            return Ok(());
        }
        "roll" => {
            let j = pop(stack)?.int()?;
            let n = pop(stack)?.int()?;
            return roll(stack, n, j);
        }
        other => {
            return Err(thrown(format!(
                "java.lang.UnsupportedOperationException: Unknown operator or name: {other}"
            )))
        }
    };
    stack.push(value);
    Ok(())
}

/// `StackOperators.Roll`: the top `n` values rolled by `j`.
fn roll(stack: &mut Vec<Value>, n: i32, j: i32) -> Result<(), Failed> {
    if j == 0 {
        return Ok(());
    }
    if n < 0 {
        return Err(thrown(format!(
            "java.lang.IllegalArgumentException: rangecheck: {n}"
        )));
    }
    let (first_count, second_count) = match j < 0 {
        true => (i64::from(n) + i64::from(j), -i64::from(j)),
        false => (i64::from(j), i64::from(n) - i64::from(j)),
    };
    let take = |stack: &mut Vec<Value>, count: i64| -> Result<Vec<Value>, Failed> {
        let count = usize::try_from(count.max(0)).unwrap_or(usize::MAX);
        let start = stack
            .len()
            .checked_sub(count)
            .ok_or_else(|| thrown("java.util.EmptyStackException"))?;
        Ok(stack.split_off(start))
    };
    let top = take(stack, first_count)?;
    let below = take(stack, second_count)?;
    stack.extend(top);
    stack.extend(below);
    Ok(())
}

/// The procedure a value holds, as `(InstructionSequence)` casts it.
fn procedure(value: Value) -> Result<Rc<Procedure>, Failed> {
    match value {
        Value::Procedure(procedure) => Ok(procedure),
        _ => Err(thrown(
            "java.lang.ClassCastException: not an InstructionSequence",
        )),
    }
}

/// `Math.round(double)`: the closest `long`, halves rounded up.
fn java_round(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    (value + 0.5).floor() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(program: &str, input: &[Value]) -> Vec<String> {
        let procedure = Procedure::parse(program).unwrap();
        let mut stack = input.to_vec();
        procedure.run(&mut stack).unwrap();
        stack
            .iter()
            .map(|value| match value {
                Value::Int(value) => format!("i{value}"),
                Value::Real(value) => format!("r{value}"),
                Value::Bool(value) => format!("b{value}"),
                Value::Procedure(_) => "proc".to_owned(),
            })
            .collect()
    }

    #[test]
    fn runs_calculator_programs_as_pdfbox_does() {
        assert_eq!(run("{ 2 3 add 4 mul }", &[]), ["i20"]);
        assert_eq!(
            run("{ 1.5 2 mul 7 2 idiv 7 2 mod }", &[]),
            ["r3", "i3", "i1"]
        );
        assert_eq!(
            run("{ dup 0.5 gt { 1 } { 0 } ifelse }", &[Value::Real(0.7)]),
            ["r0.7", "i1"]
        );
        assert_eq!(run("{ 1 2 3 3 1 roll }", &[]), ["i3", "i1", "i2"]);
        assert_eq!(run("{ 1 2 3 3 -1 roll }", &[]), ["i2", "i3", "i1"]);
        assert_eq!(
            run("{ 5 not 3 neg 1 2 bitshift }", &[]),
            ["i-5", "i-3", "i4"]
        );
        assert_eq!(run("{ 2147483647 1 add }", &[]), ["r2147483600"]);
        assert_eq!(run("{ 1 1.0 eq 0 1 index }", &[]), ["btrue", "i0", "btrue"]);
        assert_eq!(
            run("{ 0 1 atan 2.5 round -2.5 round 3.7 truncate }", &[]),
            ["r0", "r3", "r-2", "r3"]
        );
        assert!(Procedure::parse("{ . }").is_err());
        let mut stack = Vec::new();
        let unknown = Procedure::parse("{ 1 foo }").unwrap().run(&mut stack);
        assert!(matches!(unknown, Err(Failed::Thrown(_))));
    }

    #[test]
    fn reads_numbers_as_its_patterns_do() {
        assert!(is_integer("+12") && is_integer("-3") && !is_integer("1.0"));
        assert!(is_real(".5") && is_real("-1.") && is_real("1.5e-3") && is_real("."));
        assert!(!is_real("+1.5") && !is_real("1e5") && !is_real("1.5e"));
    }

    #[test]
    fn interpolates_and_clips_as_pdfbox_does() {
        assert_eq!(interpolate(0.5, 0.0, 1.0, 10.0, 20.0), 15.0);
        assert_eq!(interpolate(3.0, 2.0, 2.0, 7.0, 9.0), 7.0);
        assert_eq!(clip(1.5, 0.0, 1.0), 1.0);
    }
}
