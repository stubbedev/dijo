import org.apache.tika.detect.BOMDetector;
import org.apache.tika.detect.CompositeEncodingDetector;
import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.detect.EncodingDetector;
import org.apache.tika.detect.MetadataCharsetDetector;
import org.apache.tika.detect.html.HtmlEncodingDetector;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.ml.chardetect.MojibusterEncodingDetector;
import org.apache.tika.ml.junkdetect.JunkDetector;
import org.apache.tika.ml.junkdetect.JunkFilterEncodingDetector;
import org.apache.tika.parser.AutoDetectParser;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.csv.TextAndCSVParser;
import org.apache.tika.sax.TeeContentHandler;
import org.apache.tika.sax.ToXMLContentHandler;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.helpers.DefaultHandler;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import java.util.zip.CRC32;
import java.util.zip.GZIPOutputStream;

/**
 * Writes what Apache Tika's TextAndCSVParser makes of every input the Rust port is checked
 * against: each file of the given corpora that Tika's default detector, told the file name as
 * extractous tells it, detects as text/plain, text/csv or text/tab-separated-values, parsed
 * through AutoDetectParser as extractous parses a file; synthetic inputs written to files and
 * parsed the same way; and synthetic inputs handed to TextAndCSVParser directly with a content
 * type that declares a charset, which AutoDetectParser would replace by the detected type.
 *
 * <p>A line, gzipped, is the input's label, its base64 bytes, the content type the parser was
 * given, how it was given (auto or direct), the outcome (ok, or the exception), every metadata
 * value the parse set apart from those the input stream, the type detection and AutoDetectParser
 * set (name=value, separated by U+001F), the CRC-32 and UTF-8 length of the body's events in a
 * canonical form, how many of each element the body holds, and the CRC-32 and UTF-8 length of
 * the XHTML body as ToXMLContentHandler writes it.
 *
 * <p>The canonical form of the events between {@code <body>} and {@code </body>}, ignorable
 * whitespace left out: {@code S name[ attribute=value...]\n} for an element's start,
 * {@code E name\n} for its end, and {@code T length\ntext} for each run of adjacent characters
 * events that is not empty, with its UTF-8 length.
 *
 * <p>The encoding detectors run in the order the extractous native image's class path gives
 * them, with the native image's encoding rules ahead of Tika's jars, as in EncodingReference.java.
 */
public final class TextReference {

    private static final Set<String> TEXT_TYPES =
            Set.of("text/plain", "text/csv", "text/tab-separated-values");
    private static final Set<String> OTHERS_KEYS = Set.of(
            TikaCoreProperties.RESOURCE_NAME_KEY.getName(),
            HttpHeaders.CONTENT_LENGTH.getName(),
            TikaCoreProperties.CONTENT_TYPE_MAGIC_DETECTED.getName(),
            TikaCoreProperties.TIKA_PARSED_BY.getName(),
            TikaCoreProperties.TIKA_PARSED_BY_FULL_SET.getName());
    private static final char SEPARATOR = '\u001f';

    private record Input(String label, Path path, byte[] bytes, String contentType,
                         boolean direct) {
    }

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: TextReference <output> <label=dir>...");
            System.exit(2);
        }
        final Detector detector = new DefaultDetector();
        final List<Input> inputs = new ArrayList<>();
        for (int i = 1; i < args.length; i++) {
            final String[] root = args[i].split("=", 2);
            final Path dir = Path.of(root[1]);
            try (Stream<Path> files = Files.walk(dir)) {
                for (Path file : files.filter(Files::isRegularFile).sorted().toList()) {
                    addDetected(inputs, detector, root[0] + "/" + dir.relativize(file), file);
                }
            }
        }
        final Path synthetic = Files.createTempDirectory("text-reference");
        for (Map.Entry<String, byte[]> entry : synthetic().entrySet()) {
            final Path file = synthetic.resolve(entry.getKey());
            Files.write(file, entry.getValue());
            addDetected(inputs, detector, "synthetic/" + entry.getKey(), file);
        }
        for (Map.Entry<String, Declared> entry : declared().entrySet()) {
            inputs.add(new Input("declared/" + entry.getKey(), null, entry.getValue().bytes(),
                    entry.getValue().contentType(), true));
        }

        final EncodingDetector encodingDetector = new CompositeEncodingDetector(List.of(
                new HtmlEncodingDetector(),
                new JunkFilterEncodingDetector(JunkDetector.loadFromClasspath()),
                new MojibusterEncodingDetector(), new BOMDetector(),
                new MetadataCharsetDetector()));
        final TextAndCSVParser textParser = new TextAndCSVParser(encodingDetector);
        final Parser autoDetect = new AutoDetectParser(detector, textParser);
        final StringBuilder out = new StringBuilder();
        for (Input input : inputs) {
            final Metadata metadata = new Metadata();
            final ToXMLContentHandler xml = new ToXMLContentHandler();
            final Recorder recorder = new Recorder();
            final ContentHandler handler = new TeeContentHandler(xml, recorder);
            String status = "ok";
            try {
                if (input.direct()) {
                    metadata.set(HttpHeaders.CONTENT_TYPE, input.contentType());
                    try (TikaInputStream stream = TikaInputStream.get(input.bytes())) {
                        textParser.parse(stream, handler, metadata, new ParseContext());
                    }
                } else {
                    try (TikaInputStream stream = TikaInputStream.get(input.path(), metadata)) {
                        autoDetect.parse(stream, handler, metadata, new ParseContext());
                    }
                }
            } catch (Exception e) {
                status = "error " + e.getClass().getName() + ": " + e.getMessage();
            }
            out.append(input.label()).append('\t')
                    .append(Base64.getEncoder().encodeToString(input.bytes())).append('\t')
                    .append(input.contentType()).append('\t')
                    .append(input.direct() ? "direct" : "auto").append('\t')
                    .append(clean(status)).append('\t')
                    .append(metadata(metadata)).append('\t')
                    .append(digest(recorder.events())).append('\t')
                    .append(recorder.counts()).append('\t')
                    .append(digest(body(xml.toString()))).append('\n');
        }
        try (OutputStream file = Files.newOutputStream(Path.of(args[0]));
             GZIPOutputStream gzip = new GZIPOutputStream(file)) {
            gzip.write(out.toString().getBytes(StandardCharsets.UTF_8));
        }
        System.err.println("TextReference: " + inputs.size() + " inputs");
    }

    private static void addDetected(List<Input> inputs, Detector detector, String label,
                                    Path file) throws IOException {
        final byte[] bytes = Files.readAllBytes(file);
        if (bytes.length == 0) {
            return;
        }
        final Metadata metadata = new Metadata();
        final String type;
        try (TikaInputStream stream = TikaInputStream.get(file, metadata)) {
            type = detector.detect(stream, metadata, new ParseContext()).toString();
        }
        if (TEXT_TYPES.contains(type)) {
            inputs.add(new Input(label, file, bytes, type, false));
        }
    }

    private static String metadata(Metadata metadata) {
        final TreeMap<String, String> values = new TreeMap<>();
        for (String name : metadata.names()) {
            if (OTHERS_KEYS.contains(name)) {
                continue;
            }
            final String[] all = metadata.getValues(name);
            if (all.length != 1) {
                throw new IllegalStateException(name + " has " + all.length + " values");
            }
            values.put(name, all[0]);
        }
        return values.entrySet().stream().map(e -> {
            final String entry = e.getKey() + "=" + e.getValue();
            if (entry.chars().anyMatch(c -> c == SEPARATOR || c == '\t' || c == '\n'
                    || c == '\r')) {
                throw new IllegalStateException("metadata cannot be written: " + entry);
            }
            return entry;
        }).collect(Collectors.joining(String.valueOf(SEPARATOR)));
    }

    private static String clean(String status) {
        return status.replaceAll("[\t\r\n]", " ");
    }

    private static String body(String xml) {
        final int start = xml.indexOf("<body>");
        final int end = xml.lastIndexOf("</body>");
        if (start < 0 || end < start) {
            return "";
        }
        return xml.substring(start + "<body>".length(), end);
    }

    private static String digest(String text) {
        final byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        final CRC32 crc = new CRC32();
        crc.update(bytes);
        return String.format("%08x:%d", crc.getValue(), bytes.length);
    }

    /** The body's events in the canonical form, and how many of each element it holds. */
    private static final class Recorder extends DefaultHandler {
        private final StringBuilder events = new StringBuilder();
        private final StringBuilder text = new StringBuilder();
        private final TreeMap<String, Integer> counts = new TreeMap<>();
        private boolean inBody;

        @Override
        public void startElement(String uri, String localName, String qName,
                                 Attributes attributes) {
            if (localName.equals("body")) {
                inBody = true;
                return;
            }
            if (!inBody) {
                return;
            }
            flush();
            events.append("S ").append(localName);
            for (int i = 0; i < attributes.getLength(); i++) {
                events.append(' ').append(attributes.getLocalName(i)).append('=')
                        .append(attributes.getValue(i));
            }
            events.append('\n');
            counts.merge(localName, 1, Integer::sum);
        }

        @Override
        public void endElement(String uri, String localName, String qName) {
            if (!inBody) {
                return;
            }
            flush();
            if (localName.equals("body")) {
                inBody = false;
                return;
            }
            events.append("E ").append(localName).append('\n');
        }

        @Override
        public void characters(char[] ch, int start, int length) {
            if (inBody) {
                text.append(ch, start, length);
            }
        }

        private void flush() {
            if (text.isEmpty()) {
                return;
            }
            final String run = text.toString();
            events.append("T ").append(run.getBytes(StandardCharsets.UTF_8).length).append('\n')
                    .append(run);
            text.setLength(0);
        }

        String events() {
            flush();
            return events.toString();
        }

        String counts() {
            return counts.isEmpty() ? "-" : counts.entrySet().stream()
                    .map(e -> e.getKey() + ":" + e.getValue())
                    .collect(Collectors.joining(" "));
        }
    }

    private static byte[] bytes(String text, String charset) {
        return text.getBytes(Charset.forName(charset));
    }

    private static byte[] concat(byte[]... parts) {
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        for (byte[] part : parts) {
            out.writeBytes(part);
        }
        return out.toByteArray();
    }

    private static byte[] hex(String hex) {
        final String[] parts = hex.split(" ");
        final byte[] bytes = new byte[parts.length];
        for (int i = 0; i < parts.length; i++) {
            bytes[i] = (byte) Integer.parseInt(parts[i], 16);
        }
        return bytes;
    }

    private static final String DANISH = "Rødgrød med fløde på én skål, æbleskiver og "
            + "smørrebrød i Århus. ";

    /** Inputs written to files, by file name, which steers Tika's type detection. */
    private static Map<String, byte[]> synthetic() {
        final TreeMap<String, byte[]> inputs = new TreeMap<>();
        final String danish = DANISH.repeat(40);
        inputs.put("emoji-at-4096.txt", bytes("a".repeat(4095) + "😀 tail and "
                + danish, "UTF-8"));
        inputs.put("emoji-at-8192.txt", bytes(danish.substring(0, 2000) + "x".repeat(6191)
                + "😀😀 tail", "UTF-8"));
        inputs.put("emoji-after-sniff.txt", bytes(danish + "b".repeat(24575 - danish.length())
                + "😀 tail", "UTF-8"));
        inputs.put("controls.txt", bytes("form\ffeed, bell\u0007, nul\u0000 and ￾ in "
                + danish, "UTF-8"));
        final String[] malformed = {"ed a0 80", "ed a0 41", "ed bf bf", "c0 80", "e0 80 80",
                "f0 80 80 80", "f4 90 80 80", "80", "e2 82", "f0 9f 98", "fe", "ff 41",
                "ed a0 80 ed b0 80", "e2 28 a1", "f0 9f 41 41"};
        for (int i = 0; i < malformed.length; i++) {
            inputs.put("utf8-malformed-" + i + ".txt", concat(bytes(danish, "UTF-8"),
                    hex(malformed[i]), bytes(" end " + danish, "UTF-8")));
        }
        inputs.put("utf8-truncated-end.txt", concat(bytes(danish, "UTF-8"), hex("e2 82")));
        final String prose = "The quick brown fox jumps over the lazy dog. " + danish;
        inputs.put("bom-utf8.txt", concat(hex("ef bb bf"), bytes(prose, "UTF-8")));
        inputs.put("bom-utf8-twice.txt", concat(hex("ef bb bf ef bb bf"), bytes(prose, "UTF-8")));
        inputs.put("bom-utf16le.txt", concat(hex("ff fe"), bytes(prose, "UTF-16LE")));
        inputs.put("bom-utf16be.txt", concat(hex("fe ff"), bytes(prose, "UTF-16BE")));
        inputs.put("bom-utf16le-odd.txt", concat(hex("ff fe"), bytes(prose, "UTF-16LE"),
                hex("41")));
        inputs.put("bom-utf32le.txt", concat(hex("ff fe 00 00"), bytes(prose, "UTF-32LE")));
        inputs.put("utf16le-lone-surrogate.txt", concat(hex("ff fe"), bytes("abc", "UTF-16LE"),
                hex("00 d8 41 00"), bytes(prose, "UTF-16LE"), hex("00 dc"),
                bytes(" tail", "UTF-16LE"), hex("3d d8")));
        inputs.put("meta-charset.txt", bytes("Some notes first.\n<meta charset=\"iso-8859-15\">"
                + "\nEuro € and café, " + prose, "ISO-8859-15"));
        inputs.put("latin1.txt", bytes(prose + "Ça coûte 5 €? Non, ¤.", "windows-1252"));
        inputs.put("shift-jis.txt", bytes("日本語のテキストです。これは試験用の文書です。".repeat(10),
                "Shift_JIS"));
        inputs.put("ascii.txt", bytes("Only plain ASCII text here.\n", "US-ASCII"));
        inputs.put("one-byte.txt", hex("41"));
        inputs.put("one-high-byte.txt", hex("e6"));

        final String people = "Anne,31,Århus\nBo,42,Odense\nCarl,27,Aalborg\n".repeat(5);
        inputs.put("comma.txt", bytes("name,age,city\n" + people, "UTF-8"));
        inputs.put("comma-short.txt", bytes("name,age,city\nAnne,31,Århus\nBo,42,Odense\n",
                "UTF-8"));
        inputs.put("comma-crlf.csv", bytes(("name,age,city\n" + people).replace("\n", "\r\n"),
                "UTF-8"));
        inputs.put("comma-cr.csv", bytes(("name,age,city\n" + people).replace("\n", "\r"),
                "UTF-8"));
        inputs.put("comma-no-final-newline.csv", bytes("name,age,city\n" + people + "Dora,9,Vejle",
                "UTF-8"));
        inputs.put("semicolon.csv", bytes(("navn,alder,by\n" + people).replace(',', ';'),
                "windows-1252"));
        inputs.put("tab.txt", bytes(("navn,alder,by\n" + people).replace(',', '\t'), "UTF-8"));
        inputs.put("tab.tsv", bytes(("navn,alder,by\n" + people).replace(',', '\t'), "UTF-8"));
        inputs.put("pipe.txt", bytes(("navn,alder,by\n" + people).replace(',', '|'), "UTF-8"));
        inputs.put("spaced.csv", bytes(("navn,  alder, by\n" + people).replace(",", ",  "),
                "UTF-8"));
        final String numbers = "1,2\n3,4\n".repeat(7);
        inputs.put("quoted.csv", bytes("\"name\",\"note\"\n\"Anne\",\"says \"\"hi\"\", then, "
                + "leaves\"\n\"Bo\",\"two\nlines\"\n\"Carl\",  \"spaced\"\n\"Dora\",\"\"\n"
                + "\"Eve\",\"cr\r\nlf\"\n" + numbers, "UTF-8"));
        inputs.put("quoted-trailing.csv", bytes("a,b\n\"x\"y,z\n\"p\" ,q\n\"r\"\"\" s\",t\n"
                + numbers, "UTF-8"));
        inputs.put("quoted-trailing-sniffed.csv", bytes(numbers + "\"x\"y,z\n\"p\" ,q\n",
                "UTF-8"));
        inputs.put("quoted-unterminated.csv", bytes("a,b\n" + numbers + "3,\"open\nmore",
                "UTF-8"));
        inputs.put("quoted-eof.csv", bytes("a,b\n" + numbers + "3,\"closed\"", "UTF-8"));
        inputs.put("quoted-eof-space.csv", bytes("a,b\n" + numbers + "3,\"closed\"  ", "UTF-8"));
        inputs.put("quote-mid-cell.csv", bytes("a,b\n1,x\"y\n" + numbers, "UTF-8"));
        inputs.put("quotes-only.csv", bytes("\"one\"\n\"two\"\n".repeat(8), "UTF-8"));
        inputs.put("empty-lines.csv", bytes("a,b\n\n" + numbers.replace("\n3", "\n\n\n3")
                + "\n", "UTF-8"));
        inputs.put("trailing-delimiter.csv", bytes("a,b,\n1,2,\n".repeat(7) + "5,6,",
                "UTF-8"));
        inputs.put("trailing-empty-quoted.csv", bytes("a,\"\"\n".repeat(12), "UTF-8"));
        inputs.put("ragged.csv", bytes("a,b,c\n1,2\n3,4,5,6\n7\n" + "8,9,10\n".repeat(12),
                "UTF-8"));
        inputs.put("two-rows.csv", bytes("a,b\n1,2\n", "UTF-8"));
        inputs.put("first-row-plain.csv", bytes("title\n" + numbers, "UTF-8"));
        inputs.put("tie.txt", bytes("a,b;c\n1,2;3\n".repeat(7), "UTF-8"));
        inputs.put("tie-tab-pipe.txt", bytes("a\tb|c\n1\t2|3\n".repeat(7), "UTF-8"));
        inputs.put("prose.csv", bytes(prose + "\nAnd a second line, with a comma.\n",
                "UTF-8"));
        inputs.put("bom-comma.csv", concat(hex("ef bb bf"), bytes(numbers, "UTF-8")));
        inputs.put("utf16-comma.csv", concat(hex("ff fe"), bytes(numbers, "UTF-16LE")));
        inputs.put("emoji-cells.csv", bytes("a,b\n😀,x\n1,😀😀\n" + numbers, "UTF-8"));
        inputs.put("controls-cells.csv", bytes("a,b\n\u0001,x\u0000y\n" + numbers, "UTF-8"));
        final StringBuilder large = new StringBuilder("id,value,label\n");
        for (int i = 0; large.length() < 30000; i++) {
            large.append(i).append(',').append(i * 7).append(",\"row ").append(i)
                    .append(", ok\"\n");
        }
        inputs.put("large.csv", bytes(large.toString(), "UTF-8"));
        final StringBuilder edge = new StringBuilder();
        while (edge.length() < 20000) {
            edge.append("x,y\n");
        }
        inputs.put("mark-limit.csv", bytes(edge.substring(0, 19997) + "\"q\",1\n", "UTF-8"));
        inputs.put("mark-limit-exact.csv", bytes(edge.substring(0, 19996) + "a,b,", "UTF-8"));
        return inputs;
    }

    private record Declared(byte[] bytes, String contentType) {
    }

    /** Inputs given to TextAndCSVParser with a content type declaring a charset. */
    private static Map<String, Declared> declared() {
        final TreeMap<String, Declared> inputs = new TreeMap<>();
        final String prose = "The quick brown fox, " + DANISH;
        inputs.put("utf8-bom-declared-latin1", new Declared(concat(hex("ef bb bf"),
                bytes(prose, "UTF-8")), "text/plain; charset=ISO-8859-1"));
        inputs.put("utf16le-bom-declared-utf8", new Declared(concat(hex("ff fe"),
                bytes(prose, "UTF-16LE")), "text/plain; charset=UTF-8"));
        inputs.put("utf16be-bom-declared-utf16", new Declared(concat(hex("fe ff"),
                bytes(prose, "UTF-16BE")), "text/plain; charset=UTF-16"));
        inputs.put("utf16-double-bom-declared-utf16", new Declared(concat(hex("fe ff fe ff"),
                bytes(prose, "UTF-16BE")), "text/plain; charset=UTF-16"));
        inputs.put("latin1-declared-utf8", new Declared(bytes(prose, "windows-1252"),
                "text/plain; charset=UTF-8"));
        inputs.put("utf8-declared-windows-1252", new Declared(bytes(prose, "UTF-8"),
                "text/plain; charset=windows-1252"));
        inputs.put("utf8-declared-koi8", new Declared(bytes(prose, "UTF-8"),
                "text/plain; charset=koi8-r"));
        inputs.put("ascii-declared-latin2", new Declared(bytes("plain ascii", "US-ASCII"),
                "text/plain; charset=ISO-8859-2"));
        inputs.put("empty-declared-utf8", new Declared(new byte[0],
                "text/plain; charset=UTF-8"));
        inputs.put("csv-declared-latin1", new Declared(bytes("a;b\nÆ;Ø\n1;2\n3;4\n",
                "ISO-8859-1"), "text/csv; charset=ISO-8859-1"));
        inputs.put("csv-quoted-charset", new Declared(bytes("a,b\n1,2\n3,4\n5,6\n", "UTF-8"),
                "text/csv; charset=\"utf-8\"; header=present"));
        inputs.put("tsv-declared", new Declared(bytes("a\tb\n1\t2\n3\t4\n", "UTF-8"),
                "text/tab-separated-values; charset=UTF-8"));
        inputs.put("other-type-declared", new Declared(bytes("print('hi')\n", "UTF-8"),
                "text/x-python; charset=UTF-8; version=3"));
        return inputs;
    }
}
