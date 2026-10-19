use super::*;

/// A GRIB 2 message of temperature on the isobaric surface of `pressure` pascals, forecast six
/// hours ahead on a grid of 3 rows of 4 columns.
fn grib2(pressure: u32) -> Vec<u8> {
    let mut identification = vec![0, 0, 0, 21, 1, 0, 98, 0, 0, 2, 1, 1];
    identification.extend_from_slice(&2026u16.to_be_bytes());
    identification.extend_from_slice(&[10, 18, 12, 0, 0, 0, 1]);
    let mut grid = vec![0u8; 72];
    grid[..5].copy_from_slice(&[0, 0, 0, 72, 3]);
    grid[6..10].copy_from_slice(&12u32.to_be_bytes());
    grid[30..34].copy_from_slice(&4u32.to_be_bytes());
    grid[34..38].copy_from_slice(&3u32.to_be_bytes());
    let mut product = vec![0u8; 34];
    product[..5].copy_from_slice(&[0, 0, 0, 34, 4]);
    product[11] = 2;
    product[17] = 1;
    product[18..22].copy_from_slice(&6u32.to_be_bytes());
    product[22] = 100;
    product[24..28].copy_from_slice(&pressure.to_be_bytes());
    product[28] = 255;
    let representation = [
        0, 0, 0, 21, 5, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0,
    ];
    let data = [0, 0, 0, 17, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let mut sections = Vec::new();
    for section in [
        identification.as_slice(),
        &grid,
        &product,
        &representation,
        &data,
        END,
    ] {
        sections.extend_from_slice(section);
    }
    let mut message = b"GRIB\0\0\0\x02".to_vec();
    message.extend_from_slice(&(16 + sections.len() as u64).to_be_bytes());
    message.extend(sections);
    message
}

/// A GRIB 1 message of temperature two metres above ground on a grid of 3 rows of 4 columns.
fn grib1() -> Vec<u8> {
    let mut product = vec![0u8; 28];
    product[..3].copy_from_slice(&[0, 0, 28]);
    product[3..10].copy_from_slice(&[2, 7, 81, 255, 0x80, 11, 105]);
    product[10..12].copy_from_slice(&2u16.to_be_bytes());
    product[12..18].copy_from_slice(&[26, 10, 18, 0, 0, 1]);
    product[24] = 21;
    let mut grid = vec![0u8; 32];
    grid[..6].copy_from_slice(&[0, 0, 32, 0, 255, 0]);
    grid[6..8].copy_from_slice(&4u16.to_be_bytes());
    grid[8..10].copy_from_slice(&3u16.to_be_bytes());
    let data = [0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 8, 1];
    let length = 8 + product.len() + grid.len() + data.len() + END.len();
    let mut message = b"GRIB".to_vec();
    message.extend_from_slice(&(length as u32).to_be_bytes()[1..]);
    message.push(1);
    for section in [product.as_slice(), &grid, &data, END] {
        message.extend_from_slice(section);
    }
    message
}

#[test]
fn gathers_grib2_messages_into_variables() {
    let mut file = grib2(50_000);
    file.extend(grib2(85_000));
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&file)
        .unwrap();
    assert!(text.contains("time=1;"), "{text}");
    assert!(text.contains("lat=3;"), "{text}");
    assert!(
        text.contains("float VAR_0-0-0_L100(time=1, 2, lat=3, lon=4);"),
        "{text}"
    );
    assert!(text.contains(" :Grib2_Parameter = 0, 0, 0;"), "{text}");
    assert_eq!(metadata["grib:Originating_or_generating_Center"], ["98"]);
    assert_eq!(metadata["grib:GRIB_table_version"], ["2,1"]);
    assert_eq!(metadata["grib:file_format"], ["GRIB-2"]);
    assert_eq!(metadata["grib:reference_time"], ["2026-10-18T12:00:00Z"]);
}

#[test]
fn reads_a_grib1_message() {
    let collection = Collection::read(Bytes::new(&grib1())).unwrap();
    let (attributes, dimensions, variables) = collection.model();
    assert_eq!(attributes[3].cdl(), "file_format = \"GRIB-1\"");
    assert_eq!(
        attributes[4].cdl(),
        "reference_time = \"2026-10-18T00:00:00Z\""
    );
    assert_eq!(dimensions.len(), 3);
    assert_eq!(
        variables[0].declaration(),
        "float VAR_7-0-2-11_L105(time=1, lat=3, lon=4)"
    );
}

#[test]
fn fails_on_a_message_cut_short() {
    let whole = grib2(50_000);
    assert!(matches!(
        Collection::read(Bytes::new(&whole[..whole.len() - 4])),
        Err(crate::formats::bytes::Unread::Malformed(_))
    ));
}

#[test]
fn reads_the_messages_of_a_file_too_large_to_read_whole() {
    let file = dijo_core::large::LargeFile::padded("forecast.grib2", &grib2(50_000));
    let (text, _) = file.extract(GribParser.name());
    assert!(
        text.contains("float VAR_0-0-0_L100(time=1, lat=3, lon=4);"),
        "{text}"
    );
}
