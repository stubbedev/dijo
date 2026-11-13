//! Bencoding, the serialisation of BitTorrent's metainfo files: integers `i42e`, byte strings
//! `4:spam`, lists `l…e` and dictionaries `d…e` whose keys are byte strings.
//!
//! A document is checked whole once, with its nesting bounded by [`MAX_DEPTH`] and every length
//! by the bytes left, and is then read in place: a [`Value`] borrows its bytes, and a list or a
//! dictionary decodes its items only as they are asked for.

/// The deepest lists and dictionaries nest, as libtorrent bounds them.
pub const MAX_DEPTH: usize = 100;

/// Why reading a value of a document [`parse`] checked cannot fail.
const CHECKED: &str = "a value of a checked document reads";

/// Where and how a document is not bencoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed(pub String);

/// A value of a document checked by [`parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Integer(i64),
    Bytes(&'a [u8]),
    List(List<'a>),
    Dictionary(Dictionary<'a>),
}

/// The items of a checked list, between its `l` and its `e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct List<'a>(&'a [u8]);

/// The keys and values of a checked dictionary, between its `d` and its `e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dictionary<'a>(&'a [u8]);

/// The value bencoded in `input`, which it must be all of.
pub fn parse(input: &[u8]) -> Result<Value<'_>, Malformed> {
    let end = scan(input, 0)?;
    if end != input.len() {
        return Err(Malformed(format!(
            "{} bytes follow its value at byte {end}",
            input.len() - end
        )));
    }
    Ok(value_at(input, 0).0)
}

/// A list or dictionary being scanned, and for a dictionary whether a key comes next.
#[derive(Debug, Clone, Copy)]
enum Open {
    List,
    Dictionary { key_next: bool },
}

/// The end of the value at `at` in `input`, checked: its integers, lengths, keys and nesting.
fn scan(input: &[u8], mut at: usize) -> Result<usize, Malformed> {
    let mut open: Vec<Open> = Vec::new();
    loop {
        let byte = *input
            .get(at)
            .ok_or_else(|| Malformed(format!("it ends inside a value at byte {at}")))?;
        let key_next = matches!(open.last(), Some(Open::Dictionary { key_next: true }));
        if key_next && byte != b'e' && !byte.is_ascii_digit() {
            return Err(Malformed(format!(
                "a dictionary key at byte {at} is not a byte string"
            )));
        }
        if byte == b'e' && !open.is_empty() {
            if let Some(Open::Dictionary { key_next: false }) = open.pop() {
                return Err(Malformed(format!(
                    "a dictionary ends after a key without its value at byte {at}"
                )));
            }
            at += 1;
        } else {
            match byte {
                b'i' => at = integer(input, at)?.1,
                b'0'..=b'9' => at = bytes(input, at)?.1,
                b'l' | b'd' => {
                    if open.len() == MAX_DEPTH {
                        return Err(Malformed(format!(
                            "its lists and dictionaries nest deeper than {MAX_DEPTH} at byte {at}"
                        )));
                    }
                    open.push(match byte {
                        b'l' => Open::List,
                        _ => Open::Dictionary { key_next: true },
                    });
                    at += 1;
                    continue;
                }
                _ => {
                    return Err(Malformed(format!(
                        "the byte {byte:#04x} at byte {at} starts no value"
                    )))
                }
            }
        }
        match open.last_mut() {
            None => return Ok(at),
            Some(Open::List) => {}
            Some(Open::Dictionary { key_next }) => *key_next = !*key_next,
        }
    }
}

/// The integer `i…e` at `at` and its end: decimal digits after an optional minus, without
/// leading zeros or a negative zero, within an `i64`.
fn integer(input: &[u8], at: usize) -> Result<(i64, usize), Malformed> {
    let start = at + 1;
    let end = memchr::memchr(b'e', &input[start..])
        .map(|offset| start + offset)
        .ok_or_else(|| Malformed(format!("the integer at byte {at} has no end")))?;
    let text = &input[start..end];
    let digits = text.strip_prefix(b"-").unwrap_or(text);
    let canonical = !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && (digits == b"0" || digits[0] != b'0')
        && text != b"-0";
    let value = canonical
        .then(|| std::str::from_utf8(text).ok()?.parse::<i64>().ok())
        .flatten()
        .ok_or_else(|| {
            Malformed(format!(
                "the integer at byte {at}, {:?}, is not a decimal integer within 64 bits",
                String::from_utf8_lossy(text)
            ))
        })?;
    Ok((value, end + 1))
}

/// The byte string `length:bytes` at `at` and its end, its length within the bytes left.
fn bytes(input: &[u8], at: usize) -> Result<(&[u8], usize), Malformed> {
    let colon = memchr::memchr(b':', &input[at..])
        .map(|offset| at + offset)
        .ok_or_else(|| Malformed(format!("the byte string at byte {at} has no length")))?;
    let digits = &input[at..colon];
    let length = digits
        .iter()
        .all(u8::is_ascii_digit)
        .then(|| std::str::from_utf8(digits).ok()?.parse::<usize>().ok())
        .flatten()
        .ok_or_else(|| {
            Malformed(format!(
                "the byte string at byte {at} has the length {:?}",
                String::from_utf8_lossy(digits)
            ))
        })?;
    let start = colon + 1;
    let end = start
        .checked_add(length)
        .filter(|&end| end <= input.len())
        .ok_or_else(|| {
            Malformed(format!(
                "the byte string of {length} bytes at byte {at} runs past its end"
            ))
        })?;
    Ok((&input[start..end], end))
}

/// The checked value at `at` in `input`, and its end.
fn value_at(input: &[u8], at: usize) -> (Value<'_>, usize) {
    match input[at] {
        b'i' => {
            let (value, end) = integer(input, at).expect(CHECKED);
            (Value::Integer(value), end)
        }
        b'l' | b'd' => {
            let end = checked_end(input, at);
            let items = &input[at + 1..end - 1];
            let value = match input[at] {
                b'l' => Value::List(List(items)),
                _ => Value::Dictionary(Dictionary(items)),
            };
            (value, end)
        }
        _ => {
            let (bytes, end) = bytes(input, at).expect(CHECKED);
            (Value::Bytes(bytes), end)
        }
    }
}

/// The end of the checked list or dictionary at `at` in `input`, found by its nesting alone.
fn checked_end(input: &[u8], mut at: usize) -> usize {
    let mut depth = 0usize;
    loop {
        match input[at] {
            b'l' | b'd' => {
                depth += 1;
                at += 1;
            }
            b'e' => {
                depth -= 1;
                at += 1;
                if depth == 0 {
                    return at;
                }
            }
            b'i' => at = integer(input, at).expect(CHECKED).1,
            _ => at = bytes(input, at).expect(CHECKED).1,
        }
    }
}

/// The values of a run of checked items, in order.
#[derive(Debug, Clone)]
pub struct Values<'a> {
    items: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Values<'a> {
    type Item = Value<'a>;

    fn next(&mut self) -> Option<Value<'a>> {
        if self.at == self.items.len() {
            return None;
        }
        let (value, end) = value_at(self.items, self.at);
        self.at = end;
        Some(value)
    }
}

impl<'a> List<'a> {
    pub fn iter(self) -> Values<'a> {
        Values {
            items: self.0,
            at: 0,
        }
    }
}

impl<'a> Dictionary<'a> {
    /// Its keys and values in the order they are written.
    pub fn entries(self) -> impl Iterator<Item = (&'a [u8], Value<'a>)> {
        let mut values = Values {
            items: self.0,
            at: 0,
        };
        std::iter::from_fn(move || {
            let key = match values.next()? {
                Value::Bytes(key) => key,
                other => unreachable!("a checked dictionary's key is a byte string: {other:?}"),
            };
            let value = values
                .next()
                .expect("a checked dictionary's key has a value");
            Some((key, value))
        })
    }

    /// The value of the first entry keyed `key`.
    pub fn get(self, key: &str) -> Option<Value<'a>> {
        self.entries()
            .find(|(name, _)| *name == key.as_bytes())
            .map(|(_, value)| value)
    }
}

impl<'a> Value<'a> {
    /// What kind of value it is, as an error names it.
    pub fn kind(self) -> &'static str {
        match self {
            Value::Integer(_) => "an integer",
            Value::Bytes(_) => "a byte string",
            Value::List(_) => "a list",
            Value::Dictionary(_) => "a dictionary",
        }
    }
}
