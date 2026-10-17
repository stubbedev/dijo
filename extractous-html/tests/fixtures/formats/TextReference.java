import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.parser.AutoDetectParser;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.csv.TextAndCSVParser;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;


/**
 * Writes what Apache Tika's TextAndCSVParser makes of every input the Rust port is checked
 * against, in ParserReference's form: each file of the given corpora that Tika's default
 * detector, told the file name as extractous tells it, detects as text/plain, text/csv or
 * text/tab-separated-values, parsed through AutoDetectParser as extractous parses a file;
 * synthetic inputs written to files and parsed the same way; and synthetic inputs handed to
 * TextAndCSVParser directly with a content type that declares a charset, which AutoDetectParser
 * would replace by the detected type.
 */
public final class TextReference {

    private static final Set<String> TEXT_TYPES =
            Set.of("text/plain", "text/csv", "text/tab-separated-values");

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: TextReference <output> <label=dir>...");
            System.exit(2);
        }
        final Detector detector = new DefaultDetector();
        final List<ParserReference.Input> inputs = new ArrayList<>(ParserReference.detected(
                detector, TEXT_TYPES, java.util.Arrays.copyOfRange(args, 1, args.length)));
        inputs.addAll(ParserReference.synthetic(detector, TEXT_TYPES, synthetic()));
        for (Map.Entry<String, Declared> entry : declared().entrySet()) {
            inputs.add(new ParserReference.Input("declared/" + entry.getKey(), null,
                    entry.getValue().bytes(), entry.getValue().contentType(), true));
        }
        final TextAndCSVParser textParser =
                new TextAndCSVParser(ParserReference.encodingDetector());
        final Parser autoDetect = new AutoDetectParser(detector, textParser);
        final List<String> lines = new ArrayList<>();
        for (ParserReference.Input input : inputs) {
            lines.add(ParserReference.columns(input, textParser, autoDetect));
        }
        ParserReference.write(Path.of(args[0]), lines);
        System.err.println("TextReference: " + inputs.size() + " inputs");
    }

    private static final String DANISH = "Rødgrød med fløde på én skål, æbleskiver og "
            + "smørrebrød i Århus. ";

    /** Inputs written to files, by file name, which steers Tika's type detection. */
    private static Map<String, byte[]> synthetic() {
        final TreeMap<String, byte[]> inputs = new TreeMap<>();
        final String danish = DANISH.repeat(40);
        inputs.put("emoji-at-4096.txt", ParserReference.bytes("a".repeat(4095) + "😀 tail and "
                + danish, "UTF-8"));
        inputs.put("emoji-at-8192.txt", ParserReference.bytes(danish.substring(0, 2000) + "x".repeat(6191)
                + "😀😀 tail", "UTF-8"));
        inputs.put("emoji-after-sniff.txt", ParserReference.bytes(danish + "b".repeat(24575 - danish.length())
                + "😀 tail", "UTF-8"));
        inputs.put("controls.txt", ParserReference.bytes("form\ffeed, bell\u0007, nul\u0000 and ￾ in "
                + danish, "UTF-8"));
        final String[] malformed = {"ed a0 80", "ed a0 41", "ed bf bf", "c0 80", "e0 80 80",
                "f0 80 80 80", "f4 90 80 80", "80", "e2 82", "f0 9f 98", "fe", "ff 41",
                "ed a0 80 ed b0 80", "e2 28 a1", "f0 9f 41 41"};
        for (int i = 0; i < malformed.length; i++) {
            inputs.put("utf8-malformed-" + i + ".txt", ParserReference.concat(ParserReference.bytes(danish, "UTF-8"),
                    ParserReference.hex(malformed[i]), ParserReference.bytes(" end " + danish, "UTF-8")));
        }
        inputs.put("utf8-truncated-end.txt", ParserReference.concat(ParserReference.bytes(danish, "UTF-8"), ParserReference.hex("e2 82")));
        final String prose = "The quick brown fox jumps over the lazy dog. " + danish;
        inputs.put("bom-utf8.txt", ParserReference.concat(ParserReference.hex("ef bb bf"), ParserReference.bytes(prose, "UTF-8")));
        inputs.put("bom-utf8-twice.txt", ParserReference.concat(ParserReference.hex("ef bb bf ef bb bf"), ParserReference.bytes(prose, "UTF-8")));
        inputs.put("bom-utf16le.txt", ParserReference.concat(ParserReference.hex("ff fe"), ParserReference.bytes(prose, "UTF-16LE")));
        inputs.put("bom-utf16be.txt", ParserReference.concat(ParserReference.hex("fe ff"), ParserReference.bytes(prose, "UTF-16BE")));
        inputs.put("bom-utf16le-odd.txt", ParserReference.concat(ParserReference.hex("ff fe"), ParserReference.bytes(prose, "UTF-16LE"),
                ParserReference.hex("41")));
        inputs.put("bom-utf32le.txt", ParserReference.concat(ParserReference.hex("ff fe 00 00"), ParserReference.bytes(prose, "UTF-32LE")));
        inputs.put("utf16le-lone-surrogate.txt", ParserReference.concat(ParserReference.hex("ff fe"), ParserReference.bytes("abc", "UTF-16LE"),
                ParserReference.hex("00 d8 41 00"), ParserReference.bytes(prose, "UTF-16LE"), ParserReference.hex("00 dc"),
                ParserReference.bytes(" tail", "UTF-16LE"), ParserReference.hex("3d d8")));
        inputs.put("meta-charset.txt", ParserReference.bytes("Some notes first.\n<meta charset=\"iso-8859-15\">"
                + "\nEuro € and café, " + prose, "ISO-8859-15"));
        inputs.put("latin1.txt", ParserReference.bytes(prose + "Ça coûte 5 €? Non, ¤.", "windows-1252"));
        inputs.put("shift-jis.txt", ParserReference.bytes("日本語のテキストです。これは試験用の文書です。".repeat(10),
                "Shift_JIS"));
        inputs.put("ascii.txt", ParserReference.bytes("Only plain ASCII text here.\n", "US-ASCII"));
        inputs.put("one-byte.txt", ParserReference.hex("41"));
        inputs.put("one-high-byte.txt", ParserReference.hex("e6"));

        final String people = "Anne,31,Århus\nBo,42,Odense\nCarl,27,Aalborg\n".repeat(5);
        inputs.put("comma.txt", ParserReference.bytes("name,age,city\n" + people, "UTF-8"));
        inputs.put("comma-short.txt", ParserReference.bytes("name,age,city\nAnne,31,Århus\nBo,42,Odense\n",
                "UTF-8"));
        inputs.put("comma-crlf.csv", ParserReference.bytes(("name,age,city\n" + people).replace("\n", "\r\n"),
                "UTF-8"));
        inputs.put("comma-cr.csv", ParserReference.bytes(("name,age,city\n" + people).replace("\n", "\r"),
                "UTF-8"));
        inputs.put("comma-no-final-newline.csv", ParserReference.bytes("name,age,city\n" + people + "Dora,9,Vejle",
                "UTF-8"));
        inputs.put("semicolon.csv", ParserReference.bytes(("navn,alder,by\n" + people).replace(',', ';'),
                "windows-1252"));
        inputs.put("tab.txt", ParserReference.bytes(("navn,alder,by\n" + people).replace(',', '\t'), "UTF-8"));
        inputs.put("tab.tsv", ParserReference.bytes(("navn,alder,by\n" + people).replace(',', '\t'), "UTF-8"));
        inputs.put("pipe.txt", ParserReference.bytes(("navn,alder,by\n" + people).replace(',', '|'), "UTF-8"));
        inputs.put("spaced.csv", ParserReference.bytes(("navn,  alder, by\n" + people).replace(",", ",  "),
                "UTF-8"));
        final String numbers = "1,2\n3,4\n".repeat(7);
        inputs.put("quoted.csv", ParserReference.bytes("\"name\",\"note\"\n\"Anne\",\"says \"\"hi\"\", then, "
                + "leaves\"\n\"Bo\",\"two\nlines\"\n\"Carl\",  \"spaced\"\n\"Dora\",\"\"\n"
                + "\"Eve\",\"cr\r\nlf\"\n" + numbers, "UTF-8"));
        inputs.put("quoted-trailing.csv", ParserReference.bytes("a,b\n\"x\"y,z\n\"p\" ,q\n\"r\"\"\" s\",t\n"
                + numbers, "UTF-8"));
        inputs.put("quoted-trailing-sniffed.csv", ParserReference.bytes(numbers + "\"x\"y,z\n\"p\" ,q\n",
                "UTF-8"));
        inputs.put("quoted-unterminated.csv", ParserReference.bytes("a,b\n" + numbers + "3,\"open\nmore",
                "UTF-8"));
        inputs.put("quoted-eof.csv", ParserReference.bytes("a,b\n" + numbers + "3,\"closed\"", "UTF-8"));
        inputs.put("quoted-eof-space.csv", ParserReference.bytes("a,b\n" + numbers + "3,\"closed\"  ", "UTF-8"));
        inputs.put("quote-mid-cell.csv", ParserReference.bytes("a,b\n1,x\"y\n" + numbers, "UTF-8"));
        inputs.put("quotes-only.csv", ParserReference.bytes("\"one\"\n\"two\"\n".repeat(8), "UTF-8"));
        inputs.put("empty-lines.csv", ParserReference.bytes("a,b\n\n" + numbers.replace("\n3", "\n\n\n3")
                + "\n", "UTF-8"));
        inputs.put("trailing-delimiter.csv", ParserReference.bytes("a,b,\n1,2,\n".repeat(7) + "5,6,",
                "UTF-8"));
        inputs.put("trailing-empty-quoted.csv", ParserReference.bytes("a,\"\"\n".repeat(12), "UTF-8"));
        inputs.put("ragged.csv", ParserReference.bytes("a,b,c\n1,2\n3,4,5,6\n7\n" + "8,9,10\n".repeat(12),
                "UTF-8"));
        inputs.put("two-rows.csv", ParserReference.bytes("a,b\n1,2\n", "UTF-8"));
        inputs.put("first-row-plain.csv", ParserReference.bytes("title\n" + numbers, "UTF-8"));
        inputs.put("tie.txt", ParserReference.bytes("a,b;c\n1,2;3\n".repeat(7), "UTF-8"));
        inputs.put("tie-tab-pipe.txt", ParserReference.bytes("a\tb|c\n1\t2|3\n".repeat(7), "UTF-8"));
        inputs.put("prose.csv", ParserReference.bytes(prose + "\nAnd a second line, with a comma.\n",
                "UTF-8"));
        inputs.put("bom-comma.csv", ParserReference.concat(ParserReference.hex("ef bb bf"), ParserReference.bytes(numbers, "UTF-8")));
        inputs.put("utf16-comma.csv", ParserReference.concat(ParserReference.hex("ff fe"), ParserReference.bytes(numbers, "UTF-16LE")));
        inputs.put("emoji-cells.csv", ParserReference.bytes("a,b\n😀,x\n1,😀😀\n" + numbers, "UTF-8"));
        inputs.put("controls-cells.csv", ParserReference.bytes("a,b\n\u0001,x\u0000y\n" + numbers, "UTF-8"));
        final StringBuilder large = new StringBuilder("id,value,label\n");
        for (int i = 0; large.length() < 30000; i++) {
            large.append(i).append(',').append(i * 7).append(",\"row ").append(i)
                    .append(", ok\"\n");
        }
        inputs.put("large.csv", ParserReference.bytes(large.toString(), "UTF-8"));
        final StringBuilder edge = new StringBuilder();
        while (edge.length() < 20000) {
            edge.append("x,y\n");
        }
        inputs.put("mark-limit.csv", ParserReference.bytes(edge.substring(0, 19997) + "\"q\",1\n", "UTF-8"));
        inputs.put("mark-limit-exact.csv", ParserReference.bytes(edge.substring(0, 19996) + "a,b,", "UTF-8"));
        return inputs;
    }

    private record Declared(byte[] bytes, String contentType) {
    }

    /** Inputs given to TextAndCSVParser with a content type declaring a charset. */
    private static Map<String, Declared> declared() {
        final TreeMap<String, Declared> inputs = new TreeMap<>();
        final String prose = "The quick brown fox, " + DANISH;
        inputs.put("utf8-bom-declared-latin1", new Declared(ParserReference.concat(ParserReference.hex("ef bb bf"),
                ParserReference.bytes(prose, "UTF-8")), "text/plain; charset=ISO-8859-1"));
        inputs.put("utf16le-bom-declared-utf8", new Declared(ParserReference.concat(ParserReference.hex("ff fe"),
                ParserReference.bytes(prose, "UTF-16LE")), "text/plain; charset=UTF-8"));
        inputs.put("utf16be-bom-declared-utf16", new Declared(ParserReference.concat(ParserReference.hex("fe ff"),
                ParserReference.bytes(prose, "UTF-16BE")), "text/plain; charset=UTF-16"));
        inputs.put("utf16-double-bom-declared-utf16", new Declared(ParserReference.concat(ParserReference.hex("fe ff fe ff"),
                ParserReference.bytes(prose, "UTF-16BE")), "text/plain; charset=UTF-16"));
        inputs.put("latin1-declared-utf8", new Declared(ParserReference.bytes(prose, "windows-1252"),
                "text/plain; charset=UTF-8"));
        inputs.put("utf8-declared-windows-1252", new Declared(ParserReference.bytes(prose, "UTF-8"),
                "text/plain; charset=windows-1252"));
        inputs.put("utf8-declared-koi8", new Declared(ParserReference.bytes(prose, "UTF-8"),
                "text/plain; charset=koi8-r"));
        inputs.put("ascii-declared-latin2", new Declared(ParserReference.bytes("plain ascii", "US-ASCII"),
                "text/plain; charset=ISO-8859-2"));
        inputs.put("empty-declared-utf8", new Declared(new byte[0],
                "text/plain; charset=UTF-8"));
        inputs.put("csv-declared-latin1", new Declared(ParserReference.bytes("a;b\nÆ;Ø\n1;2\n3;4\n",
                "ISO-8859-1"), "text/csv; charset=ISO-8859-1"));
        inputs.put("csv-quoted-charset", new Declared(ParserReference.bytes("a,b\n1,2\n3,4\n5,6\n", "UTF-8"),
                "text/csv; charset=\"utf-8\"; header=present"));
        inputs.put("tsv-declared", new Declared(ParserReference.bytes("a\tb\n1\t2\n3\t4\n", "UTF-8"),
                "text/tab-separated-values; charset=UTF-8"));
        inputs.put("other-type-declared", new Declared(ParserReference.bytes("print('hi')\n", "UTF-8"),
                "text/x-python; charset=UTF-8; version=3"));
        return inputs;
    }
}
