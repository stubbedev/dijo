use extractous_core::large::LargeFile;

use super::*;

/// A file of no type any parser takes, too large to read whole, is read no further than its
/// detection reads it.
#[test]
fn writes_nothing_of_files_too_large_to_read_whole() {
    let file = LargeFile::padded("long.bin", &[0x00, 0x9F, 0x3B, 0xFE, 0x10, 0x00, 0x7F]);
    let mut text = String::new();
    file.extract_each(EmptyParser.name(), |written| text.push_str(written));
    assert_eq!(text.trim(), "");
}
