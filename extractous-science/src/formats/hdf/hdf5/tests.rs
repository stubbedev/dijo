use super::*;
use crate::formats::cdm::{DataType, Values};

/// A version 2 object header of `messages`, each a type and a body.
fn object_header(messages: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, message) in messages {
        body.push(*kind);
        body.extend_from_slice(&(message.len() as u16).to_le_bytes());
        body.push(0);
        body.extend_from_slice(message);
    }
    let mut header = b"OHDR".to_vec();
    header.extend_from_slice(&[2, 0x02]);
    header.extend_from_slice(&(body.len() as u32).to_le_bytes());
    header.extend_from_slice(&body);
    header.extend_from_slice(&[0; 4]);
    header
}

/// A version 3 attribute message of a scalar string.
fn text_attribute(name: &str, value: &str) -> Vec<u8> {
    let mut name_bytes = name.as_bytes().to_vec();
    name_bytes.push(0);
    let mut datatype = vec![0x13, 0, 0, 0];
    datatype.extend_from_slice(&(value.len() as u32).to_le_bytes());
    let dataspace = [2, 0, 0, 0];
    let mut message = vec![3, 0];
    message.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    message.extend_from_slice(&(datatype.len() as u16).to_le_bytes());
    message.extend_from_slice(&(dataspace.len() as u16).to_le_bytes());
    message.push(0);
    message.extend_from_slice(&name_bytes);
    message.extend_from_slice(&datatype);
    message.extend_from_slice(&dataspace);
    message.extend_from_slice(value.as_bytes());
    message
}

/// A file of superblock version 2 whose root group has a title and links to a dataset
/// `temperature` of 3 by 4 floats in kelvin.
fn file() -> Vec<u8> {
    let root_address = 48u64;
    let mut dataset = Vec::new();
    let mut dataspace = vec![2, 2, 0, 1];
    dataspace.extend_from_slice(&3u64.to_le_bytes());
    dataspace.extend_from_slice(&4u64.to_le_bytes());
    let mut datatype = vec![0x11, 0x20, 0x1F, 0x00];
    datatype.extend_from_slice(&4u32.to_le_bytes());
    datatype.extend_from_slice(&[0, 0, 32, 0, 23, 8, 0, 23, 127, 0, 0, 0]);
    dataset.extend(object_header(&[
        (0x01, dataspace),
        (0x03, datatype),
        (0x08, vec![3, 0, 0, 0]),
        (0x0C, text_attribute("units", "K")),
    ]));
    let link_to = |address: u64| {
        let mut link = vec![1, 0, 11];
        link.extend_from_slice(b"temperature");
        link.extend_from_slice(&address.to_le_bytes());
        link
    };
    let root_length = object_header(&[
        (0x0C, text_attribute("title", "Havtemperatur")),
        (0x06, link_to(0)),
    ])
    .len() as u64;
    let root = object_header(&[
        (0x0C, text_attribute("title", "Havtemperatur")),
        (0x06, link_to(root_address + root_length)),
    ]);
    let mut bytes = SIGNATURE.to_vec();
    bytes.extend_from_slice(&[2, 8, 8, 0]);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&root_address.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    assert_eq!(bytes.len() as u64, root_address);
    bytes.extend(root);
    bytes.extend(dataset);
    bytes
}

#[test]
fn reads_the_groups_attributes_and_the_datasets() {
    let contents = read(Bytes::new(&file())).unwrap();
    assert_eq!(
        contents.attributes,
        [Attribute {
            name: "title".to_owned(),
            values: Values::Text(vec!["Havtemperatur".to_owned()]),
        }]
    );
    let [variable] = contents.variables.as_slice() else {
        panic!("{:?}", contents.variables);
    };
    assert_eq!(variable.data_type, DataType::Float);
    assert_eq!(variable.declaration(), "float temperature(3, 4)");
    assert_eq!(variable.attributes[0].cdl(), "units = \"K\"");
}

#[test]
fn fails_on_an_object_header_cut_short() {
    let whole = file();
    let unread = read(Bytes::new(&whole[..whole.len() - 20])).unwrap_err();
    assert!(
        matches!(unread, crate::formats::bytes::Unread::Malformed(_)),
        "{unread:?}"
    );
}
