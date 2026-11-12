use flate2::{Compress, Compression, FlushCompress};

use super::cabinet::Malformed;
use super::folder::MAX_BLOCK;
use super::*;

const STORED: u16 = 0;
const MSZIP: u16 = 1;
const QUANTUM: u16 = 0x0F02;

const PREV_FLAG: u16 = 0x0001;
const NEXT_FLAG: u16 = 0x0002;
const RESERVE_PRESENT: u16 = 0x0004;
const NAME_IS_UTF: u16 = 0x0080;

const FROM_PREVIOUS: u16 = 0xFFFD;
const TO_NEXT: u16 = 0xFFFE;

/// 2021-03-04 05:06:08 as a DOS date and time.
const DATE: u16 = (41 << 9) | (3 << 5) | 4;
const TIME: u16 = (5 << 11) | (6 << 5) | 4;

/// A `CFDATA` block: its data, and what it decodes into, 0 for a block the next cabinet ends.
type Block = (Vec<u8>, u16);

/// A `CFFOLDER` with its blocks.
struct Folder {
    type_compress: u16,
    blocks: Vec<Block>,
}

/// A `CFFILE`.
struct Entry<'a> {
    name: &'a [u8],
    folder: u16,
    offset: u32,
    size: u32,
    attributes: u16,
}

fn entry(name: &str, folder: u16, offset: usize, size: usize) -> Entry<'_> {
    Entry {
        name: name.as_bytes(),
        folder,
        offset: offset as u32,
        size: size as u32,
        attributes: 0x20,
    }
}

/// The blocks of `data` stored.
fn stored(data: &[u8]) -> Vec<Block> {
    data.chunks(MAX_BLOCK)
        .map(|chunk| (chunk.to_vec(), chunk.len() as u16))
        .collect()
}

/// The blocks of `data` compressed with MSZIP, each deflated with the 32 KiB before it as its
/// dictionary.
fn mszip(data: &[u8]) -> Vec<Block> {
    let mut compress = Compress::new(Compression::best(), false);
    data.chunks(MAX_BLOCK)
        .enumerate()
        .map(|(index, chunk)| {
            compress.reset();
            let start = index * MAX_BLOCK;
            if start > 0 {
                compress
                    .set_dictionary(&data[start.saturating_sub(MAX_BLOCK)..start])
                    .unwrap();
            }
            let mut deflated = Vec::with_capacity(chunk.len() + 1024);
            let status = compress
                .compress_vec(chunk, &mut deflated, FlushCompress::Finish)
                .unwrap();
            assert_eq!(status, flate2::Status::StreamEnd);
            ([b"CK".as_slice(), &deflated].concat(), chunk.len() as u16)
        })
        .collect()
}

/// A cabinet of `folders` and `entries`, with the header `flags`, naming `previous` and `next`
/// cabinets where its flags say, its headers and blocks given reserved areas of `reserve`
/// bytes where they say so too.
fn cabinet(flags: u16, folders: &[Folder], entries: &[Entry], reserve: u8) -> Vec<u8> {
    let mut optional = Vec::new();
    if flags & RESERVE_PRESENT != 0 {
        optional.extend_from_slice(&3u16.to_le_bytes());
        optional.extend_from_slice(&[reserve, reserve]);
        optional.extend_from_slice(&[0xAA; 3]);
    }
    if flags & PREV_FLAG != 0 {
        optional.extend_from_slice(b"first.cab\0Disk 1\0");
    }
    if flags & NEXT_FLAG != 0 {
        optional.extend_from_slice(b"third.cab\0Disk 3\0");
    }
    let reserve = if flags & RESERVE_PRESENT != 0 {
        usize::from(reserve)
    } else {
        0
    };
    let folders_at = 36 + optional.len();
    let files_at = folders_at + folders.len() * (8 + reserve);
    let files: Vec<u8> = entries
        .iter()
        .flat_map(|entry| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&entry.size.to_le_bytes());
            bytes.extend_from_slice(&entry.offset.to_le_bytes());
            bytes.extend_from_slice(&entry.folder.to_le_bytes());
            bytes.extend_from_slice(&DATE.to_le_bytes());
            bytes.extend_from_slice(&TIME.to_le_bytes());
            bytes.extend_from_slice(&entry.attributes.to_le_bytes());
            bytes.extend_from_slice(entry.name);
            bytes.push(0);
            bytes
        })
        .collect();
    let mut data = Vec::new();
    let mut folder_headers = Vec::new();
    for folder in folders {
        folder_headers
            .extend_from_slice(&((files_at + files.len() + data.len()) as u32).to_le_bytes());
        folder_headers.extend_from_slice(&(folder.blocks.len() as u16).to_le_bytes());
        folder_headers.extend_from_slice(&folder.type_compress.to_le_bytes());
        folder_headers.extend(std::iter::repeat_n(0xBB, reserve));
        for (bytes, decoded) in &folder.blocks {
            data.extend_from_slice(&[0; 4]);
            data.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            data.extend_from_slice(&decoded.to_le_bytes());
            data.extend(std::iter::repeat_n(0xCC, reserve));
            data.extend_from_slice(bytes);
        }
    }
    let length = files_at + files.len() + data.len();
    let mut cab = b"MSCF\0\0\0\0".to_vec();
    cab.extend_from_slice(&(length as u32).to_le_bytes());
    cab.extend_from_slice(&[0; 4]);
    cab.extend_from_slice(&(files_at as u32).to_le_bytes());
    cab.extend_from_slice(&[0; 4]);
    cab.extend_from_slice(&[3, 1]);
    cab.extend_from_slice(&(folders.len() as u16).to_le_bytes());
    cab.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    cab.extend_from_slice(&flags.to_le_bytes());
    cab.extend_from_slice(&0x1234u16.to_le_bytes());
    cab.extend_from_slice(&0u16.to_le_bytes());
    cab.extend_from_slice(&optional);
    cab.extend_from_slice(&folder_headers);
    cab.extend_from_slice(&files);
    cab.extend_from_slice(&data);
    cab
}

/// Each file of `cab` as the parser reads it: its name, and its data, or why it is not read.
fn files(cab: &[u8]) -> Vec<(String, Result<Vec<u8>, String>)> {
    let cabinet = cabinet::read(cab).unwrap_or_else(|malformed| panic!("{malformed}"));
    let paging = Paging::new(Pages::IN_MEMORY);
    let mut reader = Reader::new(cab, &cabinet, &paging);
    cabinet
        .files
        .iter()
        .map(|file| {
            let read = match reader.read(file) {
                Ok(Read::Data(Limited::Whole(content))) => Ok(content.to_vec()),
                Ok(Read::Data(Limited::Cut(_))) => panic!("{} is cut", file.name),
                Ok(Read::Unread(reason)) => Err(reason),
                Err(failure) => panic!("{}: {failure:?}", file.name),
            };
            (file.name.clone(), read)
        })
        .collect()
}

/// Text that runs past several blocks and repeats itself across them, so that a block of it
/// compressed refers back into the block before.
fn long_text() -> Vec<u8> {
    (0..4000)
        .flat_map(|line| format!("line {line} of the long file, {}\n", line % 7).into_bytes())
        .collect()
}

fn malformed(cab: &[u8]) -> String {
    match cabinet::read(cab) {
        Err(Malformed(reason)) => reason,
        Ok(read) => panic!("read: {read:?}"),
    }
}

#[test]
fn reads_stored_and_mszip_folders_across_their_blocks() {
    let long = long_text();
    assert!(long.len() > 3 * MAX_BLOCK);
    let first = b"Stored text.\n";
    let mut packed = long.clone();
    packed.extend_from_slice(b"After the long file.\n");
    let cab = cabinet(
        0,
        &[
            Folder {
                type_compress: STORED,
                blocks: stored(&[first.as_slice(), b"second\n"].concat()),
            },
            Folder {
                type_compress: MSZIP,
                blocks: mszip(&packed),
            },
        ],
        &[
            entry("first.txt", 0, 0, first.len()),
            entry("dir\\second.txt", 0, first.len(), 7),
            entry("long.txt", 1, 0, long.len()),
            entry("after.txt", 1, long.len(), 21),
            entry("empty.txt", 1, 0, 0),
        ],
        0,
    );
    assert_eq!(
        files(&cab),
        [
            ("first.txt".to_owned(), Ok(first.to_vec())),
            ("dir/second.txt".to_owned(), Ok(b"second\n".to_vec())),
            ("long.txt".to_owned(), Ok(long)),
            (
                "after.txt".to_owned(),
                Ok(b"After the long file.\n".to_vec())
            ),
            ("empty.txt".to_owned(), Ok(Vec::new())),
        ]
    );
}

#[test]
fn rereads_a_folder_for_a_file_listed_after_one_beyond_it() {
    let text = b"abcdefghij";
    let cab = cabinet(
        RESERVE_PRESENT,
        &[Folder {
            type_compress: MSZIP,
            blocks: mszip(text),
        }],
        &[entry("late.txt", 0, 5, 5), entry("early.txt", 0, 0, 5)],
        4,
    );
    assert_eq!(
        files(&cab),
        [
            ("late.txt".to_owned(), Ok(b"fghij".to_vec())),
            ("early.txt".to_owned(), Ok(b"abcde".to_vec())),
        ]
    );
}

#[test]
fn reads_names_as_utf8_where_flagged_else_as_windows_1252() {
    let mut utf8 = entry("caf\u{e9} \u{65e5}\u{672c}.txt", 0, 0, 1);
    utf8.attributes |= NAME_IS_UTF;
    let ansi = Entry {
        name: b"na\xefve \x80.txt",
        ..entry("", 0, 1, 1)
    };
    let cab = cabinet(
        0,
        &[Folder {
            type_compress: STORED,
            blocks: stored(b"ab"),
        }],
        &[utf8, ansi],
        0,
    );
    let read = cabinet::read(&cab).unwrap();
    let names: Vec<&str> = read.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(
        names,
        ["caf\u{e9} \u{65e5}\u{672c}.txt", "na\u{ef}ve \u{20ac}.txt"]
    );
    assert_eq!(read.files[0].modified_millis, Some(1_614_834_368_000));
}

#[test]
fn records_quantum_and_undefined_folders_unread() {
    let cab = cabinet(
        0,
        &[
            Folder {
                type_compress: QUANTUM,
                blocks: stored(b"not quantum data"),
            },
            Folder {
                type_compress: 0x0007,
                blocks: stored(b"x"),
            },
            Folder {
                type_compress: 0x1603,
                blocks: stored(b"x"),
            },
        ],
        &[
            entry("quantum.txt", 0, 0, 3),
            entry("seven.txt", 1, 0, 1),
            entry("wide.txt", 2, 0, 1),
        ],
        0,
    );
    let read = files(&cab);
    assert_eq!(
        read[0].1,
        Err("its folder is compressed with Quantum, which is not decoded".to_owned())
    );
    for (name, unread) in &read[1..] {
        assert!(
            unread.as_ref().unwrap_err().contains("does not define"),
            "{name}: {unread:?}"
        );
    }
}

#[test]
fn reads_what_a_cabinet_of_a_set_holds_whole() {
    let opening = b"the end of a file begun before";
    let whole = b"a whole file";
    let spanning = b"a file the next cabinet ends";
    let mut last_folder = stored(&[whole.as_slice(), &spanning[..10]].concat());
    last_folder.push((spanning[10..].to_vec(), 0));
    let cab = cabinet(
        PREV_FLAG | NEXT_FLAG,
        &[
            Folder {
                type_compress: STORED,
                blocks: stored(opening),
            },
            Folder {
                type_compress: STORED,
                blocks: last_folder,
            },
        ],
        &[
            entry("begun.txt", FROM_PREVIOUS, 100, opening.len()),
            entry("also-in-first.txt", 0, 130, 1),
            entry("whole.txt", 1, 0, whole.len()),
            entry("spanning.txt", TO_NEXT, whole.len(), spanning.len() + 100),
            entry("listed-whole.txt", 1, whole.len(), spanning.len()),
        ],
        0,
    );
    let read = cabinet::read(&cab).unwrap();
    assert_eq!(read.previous.as_deref(), Some("first.cab"));
    assert_eq!(read.next.as_deref(), Some("third.cab"));
    assert_eq!(
        files(&cab),
        [
            (
                "begun.txt".to_owned(),
                Err("it starts in the previous cabinet, first.cab".to_owned())
            ),
            (
                "also-in-first.txt".to_owned(),
                Err("its folder starts in the previous cabinet, first.cab".to_owned())
            ),
            ("whole.txt".to_owned(), Ok(whole.to_vec())),
            (
                "spanning.txt".to_owned(),
                Err("it ends in the next cabinet, third.cab".to_owned())
            ),
            (
                "listed-whole.txt".to_owned(),
                Err("its data goes on in the next cabinet, third.cab".to_owned())
            ),
        ]
    );
    let (text, metadata) = dijo_core::Extractor::new()
        .extract_bytes_to_string(&cab)
        .unwrap();
    assert!(text.contains("a whole file"), "{text}");
    assert_eq!(metadata[PREVIOUS_CABINET], ["first.cab"]);
    assert_eq!(metadata[NEXT_CABINET], ["third.cab"]);
    let exceptions = &metadata[crate::formats::embedded::EMBEDDED_STREAM_EXCEPTION];
    assert_eq!(exceptions.len(), 4, "{exceptions:?}");
    assert!(
        exceptions[2].contains("Can't read archive stream (spanning.txt): it ends in the next"),
        "{exceptions:?}"
    );
}

#[test]
fn records_files_whose_data_does_not_decode_or_is_missing() {
    let mut corrupt = mszip(b"some text");
    corrupt[0].0[0] = b'X';
    let mut short = stored(b"0123456789");
    short[0].1 = 9;
    let cab = cabinet(
        0,
        &[
            Folder {
                type_compress: MSZIP,
                blocks: corrupt,
            },
            Folder {
                type_compress: STORED,
                blocks: stored(b"short"),
            },
            Folder {
                type_compress: STORED,
                blocks: short,
            },
        ],
        &[
            entry("corrupt.txt", 0, 0, 4),
            entry("again.txt", 0, 4, 4),
            entry("past.txt", 1, 2, 10),
            entry("mismatched.txt", 2, 0, 3),
        ],
        0,
    );
    let read = files(&cab);
    let reason = |index: usize| read[index].1.clone().unwrap_err();
    assert!(reason(0).contains("does not start with CK"), "{read:?}");
    assert_eq!(reason(1), reason(0));
    assert_eq!(
        reason(2),
        "its folder's data ends before the file's end, 12 bytes into the folder"
    );
    assert!(
        reason(3).contains("the stored block holds 10 bytes, where its header says 9"),
        "{read:?}"
    );
}

#[test]
fn refuses_headers_that_run_past_the_cabinet() {
    let cab = cabinet(
        0,
        &[Folder {
            type_compress: STORED,
            blocks: stored(b"data"),
        }],
        &[entry("a.txt", 0, 0, 4)],
        0,
    );
    assert!(malformed(b"MSCX").contains("does not start with MSCF"));
    assert!(malformed(&cab[..30]).ends_with("too short for the CFHEADER at offset 0"));
    let mut many_folders = cab.clone();
    many_folders[26..28].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(malformed(&many_folders).ends_with("too short for the 65535 CFFOLDERs at offset 36"));
    let mut many_files = cab.clone();
    many_files[28..30].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(malformed(&many_files).ends_with("too short for the 65535 CFFILEs at offset 44"));
    let mut far_files = cab.clone();
    far_files[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(malformed(&far_files).ends_with("CFFILEs at offset 4294967295"));
    let mut unnamed = cab.clone();
    unnamed.truncate(cab.len() - 4 - 8 - 1);
    assert!(malformed(&unnamed).contains("szName of CFFILE 0 at offset 60 is not ended by a NUL"));
    let mut no_folder = cab.clone();
    no_folder[52..54].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        malformed(&no_folder),
        "CFFILE 0 (a.txt) is in folder 1, of a cabinet of 1 folders"
    );
}

#[test]
fn refuses_blocks_past_the_cabinet_or_larger_than_a_frame() {
    let mut beyond = cabinet(
        0,
        &[Folder {
            type_compress: STORED,
            blocks: stored(b"data"),
        }],
        &[entry("a.txt", 0, 0, 4)],
        0,
    );
    beyond.truncate(beyond.len() - 1);
    assert!(files(&beyond)[0]
        .1
        .clone()
        .unwrap_err()
        .contains("runs past the end of the cabinet"),);
    let large = vec![b'x'; MAX_BLOCK + 1];
    let oversized = cabinet(
        0,
        &[Folder {
            type_compress: STORED,
            blocks: vec![(large, (MAX_BLOCK + 1) as u16)],
        }],
        &[entry("a.txt", 0, 0, 4)],
        0,
    );
    assert!(files(&oversized)[0]
        .1
        .clone()
        .unwrap_err()
        .contains("decodes into 32769 bytes, more than 32768"),);
}

#[test]
fn refuses_what_is_no_cabinet() {
    let configs = ParserConfigs::default();
    let Err(Decline::Fails(error)) = CabParser.prepare(b"MSCF", &TikaMetadata::new(), &configs)
    else {
        panic!("a cabinet of four bytes is prepared");
    };
    assert_eq!(
        error.to_string(),
        "the cabinet: the cabinet, 4 bytes long, is too short for the CFHEADER at offset 0"
    );
}

/// `cabinet.cab`, which `tests/fixtures/cab/cabinet.py` writes: the first cabinet of a set, of a
/// stored, an MSZIP, an LZX, a Quantum and a stored folder the next cabinet continues.
#[test]
fn reads_the_stored_mszip_and_lzx_folders_of_the_test_cabinet() {
    let cab = std::fs::read("../test_files/documents/cabinet.cab").unwrap();
    let read = files(&cab);
    let names: Vec<&str> = read.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "readme.txt",
            "notes/caf\u{e9} \u{65e5}\u{672c}.txt",
            "data/table.bin",
            "mszip.txt",
            "program.bin",
            "lzx.txt",
            "quantum.txt",
            "whole.txt",
            "spanned.txt",
        ]
    );
    let data = |index: usize| read[index].1.as_deref().unwrap();
    assert!(data(0).starts_with(b"This cabinet was written to test"));
    let (table, program) = (data(2), data(4));
    assert_eq!(
        (table.len(), crate::compress::crc32(table)),
        (99_304, 0xB66F_03F6)
    );
    assert_eq!(
        data(3),
        b"The deflated folder carries its window from one block to the next.\n"
    );
    assert_eq!(
        (program.len(), crate::compress::crc32(program)),
        (37_768, 0xA9F4_6602)
    );
    assert_eq!(
        data(5),
        b"The LZX folder is decoded frame by frame, with its calls translated back.\n"
    );
    assert_eq!(
        read[6].1,
        Err("its folder is compressed with Quantum, which is not decoded".to_owned())
    );
    assert_eq!(
        data(7),
        b"This file ends in the first cabinet of the set.\n"
    );
    assert_eq!(
        read[8].1,
        Err("it ends in the next cabinet, cabinet2.cab".to_owned())
    );
}

#[test]
fn cuts_the_file_that_decodes_past_the_cabinet_s_budget() {
    let long = long_text();
    let cab = cabinet(
        0,
        &[Folder {
            type_compress: MSZIP,
            blocks: mszip(&long),
        }],
        &[entry("long.txt", 0, 0, long.len())],
        0,
    );
    let cabinet = cabinet::read(&cab).unwrap();
    let paging = Paging::new(Pages::IN_MEMORY);
    let mut reader = Reader::new(&cab, &cabinet, &paging);
    reader.budget = 1;
    let Ok(Read::Data(Limited::Cut(content))) = reader.read(&cabinet.files[0]) else {
        panic!("the file is not cut");
    };
    assert_eq!(*content, long[..MAX_BLOCK]);
}
