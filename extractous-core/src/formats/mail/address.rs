//! Address fields as mime4j's `LenientAddressParser` reads them and as Tika's
//! `MailContentHandler` writes each address: a mailbox with a display name as
//! `Name <address>`, one without as its address, and a group as `Name: a, b;`.

use super::codec::decode_encoded_words;
use super::field::{
    copy_content, is_whitespace, parse_value, skip_all_whitespace, skip_comment, skip_whitespace,
    Cursor, Delimiters,
};
use super::Unported;

const AT: u8 = b'@';
const OPENING_BRACKET: u8 = b'<';
const CLOSING_BRACKET: u8 = b'>';
const COMMA: u8 = b',';
const COLON: u8 = b':';
const SEMICOLON: u8 = b';';

/// `Mailbox`: a display name, decoded, and the address's local part and domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mailbox {
    name: Option<String>,
    local_part: String,
    domain: Option<String>,
}

impl Mailbox {
    fn new(name: Option<String>, local_part: String, domain: Option<String>) -> Self {
        Mailbox {
            name: name.filter(|name| !name.is_empty()),
            local_part,
            domain: domain.filter(|domain| !domain.is_empty()),
        }
    }

    /// `getAddress`.
    fn address(&self) -> String {
        match &self.domain {
            Some(domain) => format!("{}@{domain}", self.local_part),
            None => self.local_part.clone(),
        }
    }

    /// `MailContentHandler.getDisplayString` of a mailbox: its name decoded once more.
    fn display(&self) -> Result<String, Unported> {
        Ok(match &self.name {
            Some(name) => format!("{} <{}>", decode_encoded_words(name)?, self.address()),
            None => self.address(),
        })
    }
}

/// `Address`: a mailbox or a named group of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Address {
    Mailbox(Mailbox),
    Group {
        name: String,
        mailboxes: Vec<Mailbox>,
    },
}

impl Address {
    /// `MailContentHandler.getDisplayString`: a group as its `toString`.
    pub(super) fn display(&self) -> Result<String, Unported> {
        match self {
            Address::Mailbox(mailbox) => mailbox.display(),
            Address::Group { name, mailboxes } => {
                let mut text = format!("{name}:");
                for (index, mailbox) in mailboxes.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    text.push(' ');
                    text.push_str(&mailbox.address());
                }
                text.push(';');
                Ok(text)
            }
        }
    }
}

/// `AddressList.flatten`: the mailboxes of the addresses, a group's in its place.
pub(super) fn flatten(addresses: Vec<Address>) -> Vec<Address> {
    addresses
        .into_iter()
        .flat_map(|address| match address {
            Address::Mailbox(_) => vec![address],
            Address::Group { mailboxes, .. } => {
                mailboxes.into_iter().map(Address::Mailbox).collect()
            }
        })
        .collect()
}

/// `LenientAddressParser.parseAddressList` of a field body, as the lenient address fields
/// parse the body of a field that is not a `RawField`: its text encoded as UTF-8.
pub(super) fn parse_address_list(body: &str) -> Result<Vec<Address>, Unported> {
    let buf = body.as_bytes();
    let mut cursor = Cursor::new(0, buf.len());
    let mut addresses = Vec::new();
    while !cursor.at_end() {
        if buf[cursor.pos] == COMMA {
            cursor.pos += 1;
        } else if let Some(address) = parse_address(buf, &mut cursor, Delimiters::of(&[COMMA]))? {
            addresses.push(address);
        }
    }
    Ok(addresses)
}

fn parse_domain(buf: &[u8], cursor: &mut Cursor, delimiters: Delimiters) -> String {
    let mut dst = String::new();
    while !cursor.at_end() {
        let current = buf[cursor.pos];
        if delimiters.contains(current) {
            break;
        } else if is_whitespace(current) {
            skip_whitespace(buf, cursor);
        } else if current == b'(' {
            skip_comment(buf, cursor);
        } else {
            copy_content(buf, cursor, delimiters, &mut dst, false);
        }
    }
    dst
}

/// `parseRoute`: the `@domain,@domain:` route of an address in angle brackets, which the
/// mailbox does not keep.
fn skip_route(buf: &[u8], cursor: &mut Cursor, delimiters: Delimiters) {
    let delimiters = Delimiters::of(&[COMMA, COLON]).with(delimiters);
    loop {
        skip_all_whitespace(buf, cursor);
        if cursor.at_end() || buf[cursor.pos] != AT {
            break;
        }
        cursor.pos += 1;
        parse_domain(buf, cursor, delimiters);
        if cursor.at_end() {
            break;
        }
        match buf[cursor.pos] {
            COMMA => cursor.pos += 1,
            COLON => {
                cursor.pos += 1;
                break;
            }
            _ => break,
        }
    }
}

fn create_mailbox(
    name: Option<String>,
    local_part: String,
    domain: Option<String>,
) -> Result<Mailbox, Unported> {
    let name = match name {
        Some(name) => Some(decode_encoded_words(&name)?),
        None => None,
    };
    Ok(Mailbox::new(name, local_part, domain))
}

/// `parseMailboxAddress`: `opening <local@domain>`, the opening text the display name.
fn parse_mailbox_address(
    opening: String,
    buf: &[u8],
    cursor: &mut Cursor,
) -> Result<Mailbox, Unported> {
    if cursor.at_end() || buf[cursor.pos] != OPENING_BRACKET {
        return create_mailbox(None, opening, None);
    }
    cursor.pos += 1;
    skip_route(buf, cursor, Delimiters::of(&[CLOSING_BRACKET]));
    let local_part = parse_value(buf, cursor, Delimiters::of(&[AT, CLOSING_BRACKET]))?;
    if cursor.at_end() {
        return create_mailbox(Some(opening), local_part, None);
    }
    match buf[cursor.pos] {
        AT | CLOSING_BRACKET => cursor.pos += 1,
        _ => return create_mailbox(Some(opening), local_part, None),
    }
    let domain = parse_domain(buf, cursor, Delimiters::of(&[CLOSING_BRACKET]));
    if cursor.at_end() {
        return create_mailbox(Some(opening), local_part, Some(domain));
    }
    if buf[cursor.pos] != CLOSING_BRACKET {
        return create_mailbox(Some(opening), local_part, Some(domain));
    }
    cursor.pos += 1;
    skip_all_whitespace(buf, cursor);
    create_mailbox(Some(opening), local_part, Some(domain))
}

/// The private `createMailbox(localPart)`: a mailbox of the opening text alone, decoded, or
/// none when it is empty.
fn bare_mailbox(local_part: String) -> Result<Option<Mailbox>, Unported> {
    if local_part.is_empty() {
        return Ok(None);
    }
    Ok(Some(Mailbox::new(
        None,
        decode_encoded_words(&local_part)?,
        None,
    )))
}

fn parse_mailbox(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
) -> Result<Option<Mailbox>, Unported> {
    let opening = parse_value(
        buf,
        cursor,
        Delimiters::of(&[AT, OPENING_BRACKET]).with(delimiters),
    )?;
    if cursor.at_end() {
        return bare_mailbox(opening);
    }
    match buf[cursor.pos] {
        OPENING_BRACKET => parse_mailbox_address(opening, buf, cursor).map(Some),
        AT => {
            cursor.pos += 1;
            let domain = parse_domain(buf, cursor, delimiters);
            Ok(Some(Mailbox::new(None, opening, Some(domain))))
        }
        _ => bare_mailbox(opening),
    }
}

fn parse_mailboxes(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
) -> Result<Vec<Mailbox>, Unported> {
    let inner = Delimiters::of(&[COMMA]).with(delimiters);
    let mut mailboxes = Vec::new();
    while !cursor.at_end() {
        let current = buf[cursor.pos];
        if delimiters.contains(current) {
            break;
        } else if current == COMMA {
            cursor.pos += 1;
        } else if let Some(mailbox) = parse_mailbox(buf, cursor, inner)? {
            mailboxes.push(mailbox);
        }
    }
    Ok(mailboxes)
}

fn parse_address(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
) -> Result<Option<Address>, Unported> {
    let opening = parse_value(
        buf,
        cursor,
        Delimiters::of(&[COLON, AT, OPENING_BRACKET]).with(delimiters),
    )?;
    if cursor.at_end() {
        return Ok(bare_mailbox(opening)?.map(Address::Mailbox));
    }
    match buf[cursor.pos] {
        OPENING_BRACKET => Ok(Some(Address::Mailbox(parse_mailbox_address(
            opening, buf, cursor,
        )?))),
        AT => {
            cursor.pos += 1;
            let domain = parse_domain(buf, cursor, delimiters);
            Ok(Some(Address::Mailbox(Mailbox::new(
                None,
                opening,
                Some(domain),
            ))))
        }
        COLON => {
            cursor.pos += 1;
            let mailboxes = parse_mailboxes(buf, cursor, Delimiters::of(&[SEMICOLON]))?;
            if !cursor.at_end() && buf[cursor.pos] == SEMICOLON {
                cursor.pos += 1;
            }
            Ok(Some(Address::Group {
                name: opening,
                mailboxes,
            }))
        }
        _ => Ok(bare_mailbox(opening)?.map(Address::Mailbox)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(body: &str) -> Vec<String> {
        parse_address_list(body)
            .unwrap()
            .iter()
            .map(|address| address.display().unwrap())
            .collect()
    }

    #[test]
    fn reads_addresses_as_mime4j_does() {
        assert_eq!(
            display("Ada Lovelace <ada@example.org>, bob@example.org"),
            ["Ada Lovelace <ada@example.org>", "bob@example.org"]
        );
        assert_eq!(
            display("\"Babbage, Charles\" (inventor) <charles@example.org>"),
            ["Babbage, Charles <charles@example.org>"]
        );
        assert_eq!(
            display("=?utf-8?q?Ren=C3=A9?= <rene@example.org>"),
            ["René <rene@example.org>"]
        );
        assert_eq!(
            display("Team: a@x.org, B <b@y.org>; c@z.org"),
            ["Team: a@x.org, b@y.org;", "c@z.org"]
        );
        assert_eq!(display("<@route,@r2:user@host>"), ["user@host"]);
        assert_eq!(
            display("undisclosed-recipients:;"),
            ["undisclosed-recipients:;"]
        );
        assert_eq!(display("just text"), ["just text"]);
    }

    #[test]
    fn flattens_groups_into_their_mailboxes() {
        let flat = flatten(parse_address_list("G: a@x, b@y;, c@z").unwrap());
        let shown: Vec<String> = flat.iter().map(|a| a.display().unwrap()).collect();
        assert_eq!(shown, ["a@x", "b@y", "c@z"]);
    }
}
