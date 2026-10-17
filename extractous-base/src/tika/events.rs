//! The SAX events of an embedded document Tika's path parses, as our Java `EventRecorder`
//! encodes them for a native parser to write into the document embedding it.

use crate::formats::Failure;

/// One SAX event of an embedded document's body, as `EventRecorder` encodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End(String),
    Characters(String),
    IgnorableWhitespace(String),
}

/// The events `EventRecorder` encoded in `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Vec<Event>, Failure> {
    let mut reader = Reader { bytes, at: 0 };
    let mut events = Vec::new();
    while let Some(&tag) = reader.bytes.get(reader.at) {
        reader.at += 1;
        events.push(match tag {
            b'S' => {
                let name = reader.string()?;
                let count = reader.int()?;
                let attributes = (0..count)
                    .map(|_| Ok((reader.string()?, reader.string()?)))
                    .collect::<Result<_, Failure>>()?;
                Event::Start { name, attributes }
            }
            b'E' => Event::End(reader.string()?),
            b'C' => Event::Characters(reader.string()?),
            b'W' => Event::IgnorableWhitespace(reader.string()?),
            other => return Err(corrupt(format!("an unknown event tag {other:#04x}"))),
        });
    }
    Ok(events)
}

fn corrupt(what: String) -> Failure {
    Failure::Malformed(format!(
        "The events Tika recorded for an embedded document hold {what}"
    ))
}

struct Reader<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], Failure> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| corrupt(format!("a field of {length} bytes past their end")))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn int(&mut self) -> Result<usize, Failure> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("four bytes were taken");
        usize::try_from(i32::from_be_bytes(bytes))
            .map_err(|_| corrupt("a negative length".to_owned()))
    }

    fn string(&mut self) -> Result<String, Failure> {
        let length = self.int()?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| corrupt(format!("text that is not UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&i32::try_from(text.len()).unwrap().to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }

    #[test]
    fn decodes_what_the_event_recorder_encodes() {
        let mut bytes = vec![b'S'];
        string(&mut bytes, "p");
        bytes.extend_from_slice(&1i32.to_be_bytes());
        string(&mut bytes, "class");
        string(&mut bytes, "ø");
        bytes.push(b'C');
        string(&mut bytes, "Hej 😀");
        bytes.push(b'W');
        string(&mut bytes, "\n");
        bytes.push(b'E');
        string(&mut bytes, "p");
        assert_eq!(
            decode(&bytes).unwrap(),
            [
                Event::Start {
                    name: "p".to_owned(),
                    attributes: vec![("class".to_owned(), "ø".to_owned())]
                },
                Event::Characters("Hej 😀".to_owned()),
                Event::IgnorableWhitespace("\n".to_owned()),
                Event::End("p".to_owned()),
            ]
        );
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(b"X").is_err());
    }
}
