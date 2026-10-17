import org.apache.tika.detect.BOMDetector;
import org.apache.tika.detect.CompositeEncodingDetector;
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
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.TeeContentHandler;
import org.apache.tika.sax.ToXMLContentHandler;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.helpers.DefaultHandler;

import java.io.IOException;
import java.io.OutputStream;
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
 * What the reference generators of the native parsers share: finding the inputs Tika's detector
 * gives a parser's types, running the parser as extractous runs it, and writing what it made of
 * each input.
 *
 * <p>A line of a reference, gzipped, starts with the input's label, its base64 bytes, the content
 * type the parser was given, how it was given (auto: through AutoDetectParser from a file of the
 * label's name, or direct), the outcome (ok, or the exception), every metadata value the parse
 * set apart from those the input stream, the type detection and AutoDetectParser set (name=value,
 * separated by U+001F), the CRC-32 and UTF-8 length of the body's events in a canonical form, how
 * many of each element the body holds, and the CRC-32 and UTF-8 length of the XHTML body as
 * ToXMLContentHandler writes it. A generator may add columns after these.
 *
 * <p>The canonical form of the events between {@code <body>} and {@code </body>}, ignorable
 * whitespace left out: {@code S name[ attribute=value...]\n} for an element's start,
 * {@code E name\n} for its end, and {@code T length\ntext} for each run of adjacent characters
 * events that is not empty, with its UTF-8 length.
 *
 * <p>The encoding detectors run in the order the extractous native image's class path gives
 * them, with the native image's encoding rules ahead of Tika's jars, as in EncodingReference.java.
 */
final class ParserReference {

    private static final Set<String> OTHERS_KEYS = Set.of(
            TikaCoreProperties.RESOURCE_NAME_KEY.getName(),
            HttpHeaders.CONTENT_LENGTH.getName(),
            TikaCoreProperties.CONTENT_TYPE_MAGIC_DETECTED.getName(),
            TikaCoreProperties.TIKA_PARSED_BY.getName(),
            TikaCoreProperties.TIKA_PARSED_BY_FULL_SET.getName());
    private static final char SEPARATOR = '\u001f';

    record Input(String label, Path path, byte[] bytes, String contentType, boolean direct) {
    }

    private ParserReference() {
    }

    /** The encoding detectors of the native image, in its order. */
    static EncodingDetector encodingDetector() throws IOException {
        return new CompositeEncodingDetector(List.of(
                new HtmlEncodingDetector(),
                new JunkFilterEncodingDetector(JunkDetector.loadFromClasspath()),
                new MojibusterEncodingDetector(), new BOMDetector(),
                new MetadataCharsetDetector()));
    }

    /** The files under each {@code label=dir} root that the detector gives one of the types. */
    static List<Input> detected(Detector detector, Set<String> types, String[] roots)
            throws IOException {
        final List<Input> inputs = new ArrayList<>();
        for (String spec : roots) {
            final String[] root = spec.split("=", 2);
            final Path dir = Path.of(root[1]);
            try (Stream<Path> files = Files.walk(dir)) {
                for (Path file : files.filter(Files::isRegularFile).sorted().toList()) {
                    addDetected(inputs, detector, types, root[0] + "/" + dir.relativize(file), file);
                }
            }
        }
        return inputs;
    }

    /** Synthetic inputs written to files of their names, which steer the type detection. */
    static List<Input> synthetic(Detector detector, Set<String> types, Map<String, byte[]> files)
            throws IOException {
        final List<Input> inputs = new ArrayList<>();
        final Path dir = Files.createTempDirectory("parser-reference");
        for (Map.Entry<String, byte[]> entry : files.entrySet()) {
            final Path file = dir.resolve(entry.getKey());
            Files.write(file, entry.getValue());
            addDetected(inputs, detector, types, "synthetic/" + entry.getKey(), file);
        }
        return inputs;
    }

    private static void addDetected(List<Input> inputs, Detector detector, Set<String> types,
                                    String label, Path file) throws IOException {
        final byte[] bytes = Files.readAllBytes(file);
        if (bytes.length == 0) {
            return;
        }
        final Metadata metadata = new Metadata();
        final String type;
        try (TikaInputStream stream = TikaInputStream.get(file, metadata)) {
            type = detector.detect(stream, metadata, new ParseContext()).toString();
        }
        if (types.contains(type)) {
            inputs.add(new Input(label, file, bytes, type, false));
        }
    }

    /**
     * The standard columns of an input's line: it parsed with {@code direct} when it is given
     * directly, and with {@code auto}, extractous's AutoDetectParser, from its file otherwise.
     */
    static String columns(Input input, Parser direct, Parser auto) {
        final Metadata metadata = new Metadata();
        final ToXMLContentHandler xml = new ToXMLContentHandler();
        final Recorder recorder = new Recorder();
        final ContentHandler handler = new TeeContentHandler(xml, recorder);
        String status = "ok";
        try {
            if (input.direct()) {
                metadata.set(HttpHeaders.CONTENT_TYPE, input.contentType());
                try (TikaInputStream stream = TikaInputStream.get(input.bytes())) {
                    direct.parse(stream, handler, metadata, new ParseContext());
                }
            } else {
                try (TikaInputStream stream = TikaInputStream.get(input.path(), metadata)) {
                    auto.parse(stream, handler, metadata, new ParseContext());
                }
            }
        } catch (Exception e) {
            status = "error " + e.getClass().getName() + ": " + e.getMessage();
        }
        return input.label() + '\t'
                + Base64.getEncoder().encodeToString(input.bytes()) + '\t'
                + input.contentType() + '\t'
                + (input.direct() ? "direct" : "auto") + '\t'
                + status.replaceAll("[\t\r\n]", " ") + '\t'
                + metadata(metadata) + '\t'
                + digest(recorder.events()) + '\t'
                + recorder.counts() + '\t'
                + digest(body(xml.toString()));
    }

    static void write(Path path, List<String> lines) throws IOException {
        try (OutputStream file = Files.newOutputStream(path);
             GZIPOutputStream gzip = new GZIPOutputStream(file)) {
            for (String line : lines) {
                gzip.write(line.getBytes(StandardCharsets.UTF_8));
                gzip.write('\n');
            }
        }
    }

    static byte[] bytes(String text, String charset) {
        return text.getBytes(java.nio.charset.Charset.forName(charset));
    }

    static byte[] concat(byte[]... parts) {
        final java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        for (byte[] part : parts) {
            out.writeBytes(part);
        }
        return out.toByteArray();
    }

    /** The bytes of space-separated hexadecimal pairs. */
    static byte[] hex(String hex) {
        final String[] parts = hex.split(" ");
        final byte[] bytes = new byte[parts.length];
        for (int i = 0; i < parts.length; i++) {
            bytes[i] = (byte) Integer.parseInt(parts[i], 16);
        }
        return bytes;
    }

    /** The CRC-32 and UTF-8 length of a text, as {@code crc:length}. */
    static String digest(String text) {
        final byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        final CRC32 crc = new CRC32();
        crc.update(bytes);
        return String.format("%08x:%d", crc.getValue(), bytes.length);
    }

    private static String metadata(Metadata metadata) {
        final TreeMap<String, String> values = new TreeMap<>();
        for (String name : metadata.names()) {
            if (OTHERS_KEYS.contains(name)) {
                continue;
            }
            values.put(name, String.join(String.valueOf(SEPARATOR) + name + "=",
                    metadata.getValues(name)));
        }
        return values.entrySet().stream().map(e -> {
            final String entry = e.getKey() + "=" + e.getValue();
            if (entry.chars().anyMatch(c -> c == '\t' || c == '\n' || c == '\r')) {
                throw new IllegalStateException("metadata cannot be written: " + entry);
            }
            return entry;
        }).collect(Collectors.joining(String.valueOf(SEPARATOR)));
    }

    private static String body(String xml) {
        final int start = xml.indexOf("<body>");
        final int end = xml.lastIndexOf("</body>");
        if (start < 0 || end < start) {
            return "";
        }
        return xml.substring(start + "<body>".length(), end);
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
}
