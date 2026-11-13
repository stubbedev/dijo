"""Writes test_files/documents/lighthouse.torrent, a multi-file BitTorrent metainfo file as
mktorrent 1.1 writes one, with what BitTorrentParser reads and what it leaves out:

- the announce URL and an announce-list of two tiers, the first repeating it;
- a comment with a character beyond ASCII, the creating client and the creation date;
- the info dictionary of four files in two directories and at the top, under a name beyond
  ASCII, with the piece length, the SHA-1 hashes of the pieces of the files' bytes and the
  private flag.

The keys of each dictionary are sorted by their bytes, as bencoding requires. The files' bytes
are generated, so the piece hashes are real; a client given the files can verify them:

    mktorrent -a http://tracker.example.org:6969/announce \\
        -a udp://tracker.example.net:1337/announce \\
        -c "Logbooks and charts of the lighthouse at Skagen, 1858–1958" -l 14 -p \\
        "Skagen Fyr årbøger"

Usage: lighthouse.py OUTPUT.torrent
"""

import hashlib
import sys

PIECE_LENGTH = 1 << 14


def bencode(value):
    """The bencoding of `value`: an int, bytes, str (in UTF-8), list or dict."""
    if isinstance(value, int):
        return b"i%de" % value
    if isinstance(value, str):
        value = value.encode("utf-8")
    if isinstance(value, bytes):
        return b"%d:%s" % (len(value), value)
    if isinstance(value, list):
        return b"l" + b"".join(bencode(item) for item in value) + b"e"
    entries = sorted((key.encode("utf-8"), item) for key, item in value.items())
    return b"d" + b"".join(bencode(key) + bencode(item) for key, item in entries) + b"e"


def contents(seed, length):
    """`length` bytes of text generated from `seed`."""
    line = ("%s: the lamp was lit at dusk and put out at dawn.\n" % seed).encode("ascii")
    return (line * (length // len(line) + 1))[:length]


FILES = [
    (["README.txt"], contents("readme", 412)),
    (["charts", "skagen-1858.txt"], contents("chart", 20000)),
    (["logbooks", "1858.txt"], contents("1858", 9000)),
    (["logbooks", "1958.txt"], contents("1958", 3100)),
]


def pieces(data):
    """The SHA-1 hashes of the pieces of `data`, concatenated."""
    return b"".join(
        hashlib.sha1(data[at : at + PIECE_LENGTH]).digest()
        for at in range(0, len(data), PIECE_LENGTH)
    )


TORRENT = {
    "announce": "http://tracker.example.org:6969/announce",
    "announce-list": [
        ["http://tracker.example.org:6969/announce"],
        ["udp://tracker.example.net:1337/announce"],
    ],
    "comment": "Logbooks and charts of the lighthouse at Skagen, 1858–1958",
    "created by": "mktorrent 1.1",
    "creation date": 1763035200,
    "info": {
        "files": [{"length": len(data), "path": path} for path, data in FILES],
        "name": "Skagen Fyr årbøger",
        "piece length": PIECE_LENGTH,
        "pieces": pieces(b"".join(data for _, data in FILES)),
        "private": 1,
    },
}

if __name__ == "__main__":
    with open(sys.argv[1], "wb") as output:
        output.write(bencode(TORRENT))
