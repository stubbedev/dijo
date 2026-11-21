"""Writes harbour.tif, a GeoTIFF of a 4 by 4 grey raster of a harbour chart, with what
metadata-extractor 2.22 reads into its GeoTIFF directory:

- the model pixel scale and tie point;
- a GeoKey directory of version 1.1.0 whose keys are held inline (the model and raster types,
  the geographic type, the angular and linear units and an unknown projection code), in the
  key directory itself (a vertical datum pair), in the double parameters (the semi-major axis
  and the inverse flattening) and in the ASCII parameters (two citations, the first ended by
  the `|` GeoTIFF separates them with);
- GDAL's no-data value.

harbour-reference.tsv holds the image metadata `tika-app-4.2.0.jar -m harbour.tif` reads of it,
each name with its values, but the file's own.

Usage: harbour.py OUTPUT.tif
"""

import struct
import sys

SHORT, ASCII, LONG, DOUBLE = 3, 2, 4, 12
WIDTH = HEIGHT = 4

CITATION = b"WGS 84 / harbour chart|"
GEOG_CITATION = b"WGS 84|"
ASCII_PARAMS = CITATION + GEOG_CITATION + b"\0"
DOUBLE_PARAMS = [6378137.0, 298.257223563]

KEYS = [
    (1024, 0, 1, 2),  # GTModelTypeGeoKey: geographic
    (1025, 0, 1, 1),  # GTRasterTypeGeoKey: pixel is area
    (1026, 34737, len(CITATION), 0),  # GTCitationGeoKey
    (2048, 0, 1, 4326),  # GeographicTypeGeoKey
    (2049, 34737, len(GEOG_CITATION), len(CITATION)),  # GeogCitationGeoKey
    (2054, 0, 1, 9102),  # GeogAngularUnitsGeoKey: degree
    (2057, 34736, 1, 0),  # GeogSemiMajorAxisGeoKey
    (2059, 34736, 1, 1),  # GeogInvFlatteningGeoKey
    (3074, 0, 1, 12345),  # ProjectionGeoKey: a code with no name
    (3076, 0, 1, 9001),  # ProjLinearUnitsGeoKey: metre
    (4098, 34735, 2, 0),  # VerticalDatumGeoKey: a pair after the keys, offset set below
]


def key_directory():
    header = [1, 1, 0, len(KEYS)]
    pair_at = 4 + 4 * len(KEYS)
    entries = []
    for key, location, count, offset in KEYS:
        if location == 34735:
            offset = pair_at
        entries += [key, location, count, offset]
    return header + entries + [5030, 7]


def main(output):
    pixels = bytes(range(0, 256, 16))
    shorts = key_directory()
    blobs = [
        ("pixels", pixels),
        ("scale", struct.pack("<3d", 0.25, 0.25, 0.0)),
        ("tie", struct.pack("<6d", 0, 0, 0, 10.58, 57.72, 0)),
        ("keys", struct.pack("<%dH" % len(shorts), *shorts)),
        ("doubles", struct.pack("<%dd" % len(DOUBLE_PARAMS), *DOUBLE_PARAMS)),
        ("ascii", ASCII_PARAMS),
    ]
    entries_count = 16
    ifd_size = 2 + 12 * entries_count + 4
    at = 8 + ifd_size
    offsets = {}
    data = b""
    for name, blob in blobs:
        if len(data) % 2:
            data += b"\0"
        offsets[name] = at + len(data)
        data += blob
    entries = [
        (256, SHORT, 1, WIDTH),
        (257, SHORT, 1, HEIGHT),
        (258, SHORT, 1, 8),
        (259, SHORT, 1, 1),
        (262, SHORT, 1, 1),
        (273, LONG, 1, offsets["pixels"]),
        (277, SHORT, 1, 1),
        (278, SHORT, 1, HEIGHT),
        (279, LONG, 1, len(pixels)),
        (284, SHORT, 1, 1),
        (33550, DOUBLE, 3, offsets["scale"]),
        (33922, DOUBLE, 6, offsets["tie"]),
        (34735, SHORT, len(shorts), offsets["keys"]),
        (34736, DOUBLE, len(DOUBLE_PARAMS), offsets["doubles"]),
        (34737, ASCII, len(ASCII_PARAMS), offsets["ascii"]),
        (42113, ASCII, 4, b"255\0"),
    ]
    assert len(entries) == entries_count
    ifd = struct.pack("<H", entries_count)
    for tag, kind, count, value in entries:
        if kind == SHORT and count == 1:
            ifd += struct.pack("<HHIHH", tag, kind, count, value, 0)
        elif isinstance(value, bytes):
            ifd += struct.pack("<HHI", tag, kind, count) + value.ljust(4, b"\0")
        else:
            ifd += struct.pack("<HHII", tag, kind, count, value)
    ifd += struct.pack("<I", 0)
    with open(output, "wb") as out:
        out.write(b"II*\0" + struct.pack("<I", 8) + ifd + data)


if __name__ == "__main__":
    main(sys.argv[1])
