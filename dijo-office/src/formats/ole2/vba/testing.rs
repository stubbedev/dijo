//! VBA projects for the tests of this crate and those built on it: MS-OVBA's structures written
//! as Office writes them, uncompressed in raw chunks.

use super::CHUNK;
use crate::ole2::{write, Node, Storage};

/// `bytes` as one raw chunk after another, as MS-OVBA stores what it does not compress.
pub fn raw(bytes: &[u8]) -> Vec<u8> {
    let mut compressed = vec![0x01];
    for chunk in bytes.chunks(CHUNK) {
        compressed.extend((0x3000 | (chunk.len() as u16 - 1)).to_le_bytes());
        compressed.extend_from_slice(chunk);
    }
    compressed
}

/// A record of a `dir` stream of an id and the bytes after its size.
pub fn record(id: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_le_bytes().to_vec();
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// `text` in UTF-16LE.
pub fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A module of a project's `dir` stream: the name of its stream in the project's code page, its
/// name in Unicode, and where its source starts in its stream.
#[derive(Debug, Clone, Copy)]
pub struct DirModule<'a> {
    pub name: &'a [u8],
    pub unicode: &'a str,
    pub offset: u32,
}

/// The decompressed `dir` stream of a project in `code_page` of `modules`, with the
/// `PROJECTCOMPATVERSION` record Office 2010 and later write.
pub fn dir_of(code_page: u16, modules: &[DirModule]) -> Vec<u8> {
    let mut bytes = record(0x0003, &code_page.to_le_bytes());
    bytes.extend(record(0x004A, &2u32.to_le_bytes()));
    bytes.extend(record(0x000F, &(modules.len() as u16).to_le_bytes()));
    for module in modules {
        bytes.extend(record(0x0019, module.name));
        bytes.extend(record(0x0047, &utf16(module.unicode)));
        bytes.extend(record(0x001A, module.name));
        bytes.extend(0x0032u16.to_le_bytes());
        let unicode = utf16(module.unicode);
        bytes.extend((unicode.len() as u32).to_le_bytes());
        bytes.extend(unicode);
        bytes.extend(record(0x0031, &module.offset.to_le_bytes()));
        bytes.extend(0x002Bu16.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
    }
    bytes.extend(0x0010u16.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes
}

/// The decompressed `dir` stream of a project in `code_page` of the modules `streams`, named in
/// ASCII, each with where its source starts in its stream.
pub fn dir(code_page: u16, streams: &[(&str, u32)]) -> Vec<u8> {
    let modules: Vec<DirModule> = streams
        .iter()
        .map(|&(name, offset)| DirModule {
            name: name.as_bytes(),
            unicode: name,
            offset,
        })
        .collect();
    dir_of(code_page, &modules)
}

/// A module's stream: a performance cache of `cache` bytes, then `source` compressed.
pub fn module(cache: usize, source: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xAB; cache];
    bytes.extend(raw(source));
    bytes
}

/// An entry of a storage named `name`.
pub fn entry(name: &str, node: Node<'static>) -> (Vec<u16>, Node<'static>) {
    (name.encode_utf16().collect(), node)
}

/// A compound document whose project storage holds the `VBA` storage of `vba` and the PROJECT
/// stream `project`, as a `vbaProject.bin` does.
pub fn document(vba: Vec<(Vec<u16>, Node<'static>)>, project: &str) -> Vec<u8> {
    write(&Storage {
        class_id: [0; 16],
        entries: vec![entry(
            "_VBA_PROJECT_CUR",
            Node::Storage(Storage {
                class_id: [0; 16],
                entries: vec![
                    entry("PROJECT", Node::Owned(project.as_bytes().to_vec())),
                    entry(
                        "VBA",
                        Node::Storage(Storage {
                            class_id: [0; 16],
                            entries: vba,
                        }),
                    ),
                ],
            }),
        )],
    })
    .expect("a VBA project writes")
}

/// A project in code page 1252 of the standard modules `modules`, each a name and its source in
/// ASCII.
pub fn project(modules: &[(&str, &str)]) -> Vec<u8> {
    let offsets: Vec<(&str, u32)> = modules.iter().map(|&(name, _)| (name, 4)).collect();
    let mut vba = vec![
        entry("dir", Node::Owned(raw(&dir(1252, &offsets)))),
        entry("_VBA_PROJECT", Node::Owned(vec![0xCC, 0x61, 0xFF, 0xFF])),
    ];
    let mut properties = String::from("ID=\"{00000000-0000-0000-0000-000000000000}\"\r\n");
    for &(name, source) in modules {
        vba.push(entry(name, Node::Owned(module(4, source.as_bytes()))));
        properties.push_str(&format!("Module={name}\r\n"));
    }
    document(vba, &properties)
}
