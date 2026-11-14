"""Writes Parquet files of a harbour's berth log with pyarrow (24.0.0 wrote the committed files),
of what ParquetParser reads:

- "log", test_files/documents/harbour.parquet: two row groups of three rows, in data pages of
  version 1, the key-value metadata "harbour" and "compiled-by", and columns of a required INT32,
  dictionary-encoded strings (Snappy) with nulls among them (Zstandard), PLAIN strings with nulls
  (Zstandard), DELTA_LENGTH_BYTE_ARRAY (Snappy) and DELTA_BYTE_ARRAY (Zstandard) strings, an
  uncompressed DOUBLE, a DELTA_BINARY_PACKED INT64, a DATE, a TIMESTAMP in milliseconds adjusted
  to UTC, a BOOLEAN with a null, a DECIMAL(9, 2) and a list of strings with an empty and a null
  list;
- "pages-v2", dijo-small/tests/fixtures/parquet/pages-v2.parquet: one row group in data pages of
  version 2, of strings compressed with gzip, Brotli and LZ4_RAW, each a word repeated so that
  its page compresses smaller than it is (pyarrow writes a page that does not uncompressed), a
  BYTE_STREAM_SPLIT FLOAT, an RLE BOOLEAN, an unsigned INT32, a TIME in microseconds, a
  TIMESTAMP in nanoseconds not adjusted to UTC, a FLOAT16 and a struct of two DOUBLEs with a
  null struct.

Usage: harbour.py log|pages-v2 OUTPUT.parquet
"""

import datetime
import decimal
import sys

import pyarrow as pa
import pyarrow.parquet as pq


def log(output):
    schema = pa.schema(
        [
            pa.field("berth", pa.int32(), nullable=False),
            pa.field("vessel", pa.string()),
            pa.field("flag", pa.string()),
            pa.field("skipper", pa.string()),
            pa.field("remarks", pa.string()),
            pa.field("cargo", pa.string()),
            pa.field("length_m", pa.float64()),
            pa.field("tonnage", pa.int64()),
            pa.field("arrived", pa.date32()),
            pa.field("moored_at", pa.timestamp("ms", tz="UTC")),
            pa.field("paid", pa.bool_()),
            pa.field("fee", pa.decimal128(9, 2)),
            pa.field("crew", pa.list_(pa.string())),
        ]
    )
    utc = datetime.timezone.utc
    table = pa.table(
        {
            "berth": [1, 2, 3, 4, 5, 6],
            "vessel": ["Ellen Marie", "Havørn", "Ellen Marie", "Skagerrak", "Havørn", "Nordlys"],
            "flag": ["DK", "NO", None, "DK", "NO", "SE"],
            "skipper": ["Jens Holm", None, "Jens Holm", "Åse Lund", None, "Pia Berg"],
            "remarks": [
                "fenders split",
                "",
                "lines doubled at high water",
                "waiting for the pilot",
                "left before dawn",
                "arrived under sail",
            ],
            "cargo": [
                "herring, salted",
                "herring, smoked",
                "timber",
                "timber, pine",
                "coal",
                "coal and coke",
            ],
            "length_m": [21.5, 14.25, 21.5, 48.0, 14.25, 1e-05],
            "tonnage": [120, 45, 118, 1650, 46, -3],
            "arrived": [
                datetime.date(2024, 3, 1),
                datetime.date(2024, 3, 2),
                datetime.date(2024, 3, 2),
                datetime.date(1999, 12, 31),
                None,
                datetime.date(2024, 3, 9),
            ],
            "moored_at": [
                datetime.datetime(2024, 3, 1, 6, 30, tzinfo=utc),
                datetime.datetime(2024, 3, 2, 18, 5, 12, 250000, tzinfo=utc),
                None,
                datetime.datetime(1999, 12, 31, 23, 59, 59, tzinfo=utc),
                datetime.datetime(2024, 3, 5, 4, 0, tzinfo=utc),
                datetime.datetime(2024, 3, 9, 12, 0, tzinfo=utc),
            ],
            "paid": [True, False, None, True, True, False],
            "fee": [
                decimal.Decimal("150.00"),
                decimal.Decimal("75.50"),
                decimal.Decimal("-0.05"),
                decimal.Decimal("1234567.89"),
                None,
                decimal.Decimal("0.00"),
            ],
            "crew": [
                ["Jens", "Ole"],
                [],
                None,
                ["Åse", "Karl", "Nils"],
                ["Ida"],
                ["Pia"],
            ],
        },
        schema=schema,
    )
    with pq.ParquetWriter(
        output,
        schema,
        data_page_version="1.0",
        store_schema=False,
        use_dictionary=["vessel", "flag"],
        column_encoding={
            "berth": "PLAIN",
            "skipper": "PLAIN",
            "remarks": "DELTA_LENGTH_BYTE_ARRAY",
            "cargo": "DELTA_BYTE_ARRAY",
            "length_m": "PLAIN",
            "tonnage": "DELTA_BINARY_PACKED",
            "arrived": "PLAIN",
            "moored_at": "PLAIN",
            "paid": "PLAIN",
            "fee": "PLAIN",
            "crew.list.element": "PLAIN",
        },
        compression={
            "berth": "snappy",
            "vessel": "snappy",
            "flag": "zstd",
            "skipper": "zstd",
            "remarks": "snappy",
            "cargo": "zstd",
            "length_m": "none",
            "tonnage": "snappy",
            "arrived": "zstd",
            "moored_at": "snappy",
            "paid": "zstd",
            "fee": "snappy",
            "crew.list.element": "zstd",
        },
    ) as writer:
        writer.write_table(table, row_group_size=3)
        writer.add_key_value_metadata({"harbour": "Skagen", "compiled-by": "the harbour master"})


def repeated(word):
    """`word` eight times, for the page of its column to compress smaller than it is."""
    return " ".join([word] * 8)


def pages_v2(output):
    position = pa.struct([("lat", pa.float64()), ("lon", pa.float64())])
    table = pa.table(
        {
            "gzip_text": [repeated("Skagen"), repeated("Hirtshals"), None],
            "brotli_text": [repeated("Grenen"), None, repeated("Råbjerg Mile")],
            "lz4_text": [repeated("Vippefyr"), repeated("Fyrtårn"), repeated("Bundgarn")],
            "depth": pa.array([4.5, -0.25, 12.0], pa.float32()),
            "lit": [True, None, False],
            "keeper_id": pa.array([4294967295, 7, None], pa.uint32()),
            "lit_at": pa.array(
                [datetime.time(16, 45), datetime.time(0, 0, 0, 1), None], pa.time64("us")
            ),
            "logged": pa.array([1709280000123456789, None, 0], pa.timestamp("ns")),
            "lens": pa.array([1.5, None, -2.0], pa.float16()),
            "position": pa.array(
                [{"lat": 57.7366, "lon": 10.6325}, None, {"lat": 57.59, "lon": None}], position
            ),
        }
    )
    pq.write_table(
        table,
        output,
        data_page_version="2.0",
        store_schema=False,
        use_dictionary=False,
        column_encoding={"depth": "BYTE_STREAM_SPLIT", "lit": "RLE"},
        compression={
            "gzip_text": "gzip",
            "brotli_text": "brotli",
            "lz4_text": "lz4",
            "depth": "none",
            "lit": "zstd",
            "keeper_id": "snappy",
            "lit_at": "snappy",
            "logged": "snappy",
            "lens": "snappy",
            "position.lat": "zstd",
            "position.lon": "none",
        },
    )


WRITERS = {"log": log, "pages-v2": pages_v2}

if __name__ == "__main__":
    WRITERS[sys.argv[1]](sys.argv[2])
