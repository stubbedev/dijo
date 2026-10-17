package ai.yobix;

import org.apache.tika.exception.WriteLimitReachedException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.ToTextContentHandler;
import org.apache.tika.sax.ToXMLContentHandler;
import org.apache.tika.sax.WriteOutContentHandler;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;
import org.xml.sax.helpers.AttributesImpl;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Runs the parser calls of scripts.txt through Tika's XHTMLContentHandler and the handler chains
 * extractous puts behind Tika's parsers (TikaNativeMain.parseToString and ParsingInputStream),
 * and writes what each chain outputs to chains.tsv, which the Rust port must reproduce.
 * `just handler-reference` runs it with our handler classes copied beside it.
 */
public final class HandlerChainReference {

    static final int LARGE_LIMIT = 1_000_000;
    static final int SMALL_LIMIT = 40;

    record Result(String output, boolean stopped) {
    }

    interface Chain {
        Result run(List<String[]> script) throws IOException;
    }

    public static void main(String[] args) throws Exception {
        final Map<String, List<String[]>> scripts = read(Path.of(args[0]));
        final Map<String, Chain> chains = new LinkedHashMap<>();
        chains.put("stream-text", HandlerChainReference::streamText);
        chains.put("stream-xml", HandlerChainReference::streamXml);
        for (final int limit : new int[]{LARGE_LIMIT, SMALL_LIMIT}) {
            chains.put("string-text-" + limit, script -> stringText(script, limit));
            chains.put("string-xml-" + limit, script -> stringXml(script, limit));
        }
        final StringBuilder out = new StringBuilder();
        for (final Map.Entry<String, List<String[]>> script : scripts.entrySet()) {
            for (final Map.Entry<String, Chain> chain : chains.entrySet()) {
                final Result result = chain.getValue().run(script.getValue());
                out.append(script.getKey()).append('\t').append(chain.getKey()).append('\t')
                        .append(result.stopped()).append('\t').append(escape(result.output()))
                        .append('\n');
            }
        }
        Files.writeString(Path.of(args[1]), out.toString(), StandardCharsets.UTF_8);
    }

    static Result streamText(List<String[]> script) throws IOException {
        final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        final Writer writer = new OutputStreamWriter(bytes, StandardCharsets.UTF_8);
        final boolean stopped = play(script, new BodyContentHandler(new AnchoredBlockHandler(
                new BlockBreakHandler(new ToTextContentHandler(writer)))));
        writer.flush();
        return new Result(bytes.toString(StandardCharsets.UTF_8), stopped);
    }

    static Result streamXml(List<String[]> script) throws IOException {
        final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        final boolean stopped = play(script, new AnchoredBlockHandler(new WellFormedContentHandler(
                new ToXMLContentHandler(bytes, "UTF-8"))));
        return new Result(bytes.toString(StandardCharsets.UTF_8), stopped);
    }

    static Result stringText(List<String[]> script, int limit) {
        final WriteOutContentHandler handler = new WriteOutContentHandler(limit);
        final boolean stopped = play(script, new BodyContentHandler(new BlockBreakHandler(handler)));
        return new Result(handler.toString(), stopped);
    }

    static Result stringXml(List<String[]> script, int limit) {
        final WriteOutContentHandler handler = new WriteOutContentHandler(
                new WellFormedContentHandler(new ToXMLContentHandler()), limit);
        final boolean stopped = play(script, handler);
        return new Result(handler.toString(), stopped);
    }

    /** Plays a script on an XHTMLContentHandler over handler; true when the write limit stopped it. */
    static boolean play(List<String[]> script, ContentHandler handler) {
        final Metadata metadata = new Metadata();
        int call = 0;
        while (call < script.size() && script.get(call)[0].equals("meta")) {
            metadata.addTrusted(script.get(call)[1], script.get(call)[2]);
            call++;
        }
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        try {
            for (; call < script.size(); call++) {
                final String[] fields = script.get(call);
                switch (fields[0]) {
                    case "start-document" -> xhtml.startDocument();
                    case "end-document" -> xhtml.endDocument();
                    case "start" -> {
                        final AttributesImpl attributes = new AttributesImpl();
                        for (int i = 2; i + 1 < fields.length; i += 2) {
                            attributes.addAttribute("", fields[i], fields[i], "CDATA", fields[i + 1]);
                        }
                        xhtml.startElement(fields[1], attributes);
                    }
                    case "end" -> xhtml.endElement(fields[1]);
                    case "chars" -> xhtml.characters(fields[1]);
                    case "ws" -> xhtml.ignorableWhitespace(fields[1].toCharArray(), 0, fields[1].length());
                    case "newline" -> xhtml.newline();
                    case "prefix" -> xhtml.startPrefixMapping(fields[1], fields[2]);
                    case "element" -> xhtml.element(fields[1], fields.length > 2 ? fields[2] : "");
                    default -> throw new IllegalArgumentException("Unknown call " + fields[0]);
                }
            }
            return false;
        } catch (SAXException e) {
            if (WriteLimitReachedException.isWriteLimitReached(e)) {
                return true;
            }
            throw new IllegalStateException(e);
        }
    }

    static Map<String, List<String[]>> read(Path path) throws IOException {
        final Map<String, List<String[]>> scripts = new LinkedHashMap<>();
        List<String[]> current = null;
        for (final String line : Files.readAllLines(path, StandardCharsets.UTF_8)) {
            if (line.isEmpty() || line.startsWith("#")) {
                continue;
            }
            if (line.startsWith("== ")) {
                current = new ArrayList<>();
                scripts.put(line.substring(3), current);
                continue;
            }
            final String[] fields = line.split("\t", -1);
            for (int i = 0; i < fields.length; i++) {
                fields[i] = unescape(fields[i]);
            }
            current.add(fields);
        }
        return scripts;
    }

    static String unescape(String field) {
        final StringBuilder out = new StringBuilder();
        for (int i = 0; i < field.length(); i++) {
            final char c = field.charAt(i);
            if (c != '\\') {
                out.append(c);
                continue;
            }
            final char next = field.charAt(++i);
            switch (next) {
                case 'n' -> out.append('\n');
                case 't' -> out.append('\t');
                case 'r' -> out.append('\r');
                case '\\' -> out.append('\\');
                case 'u' -> {
                    out.append((char) Integer.parseInt(field.substring(i + 1, i + 5), 16));
                    i += 4;
                }
                case 'U' -> {
                    out.appendCodePoint(Integer.parseInt(field.substring(i + 1, i + 9), 16));
                    i += 8;
                }
                default -> throw new IllegalArgumentException("Unknown escape \\" + next);
            }
        }
        return out.toString();
    }

    static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r")
                .replace("\t", "\\t");
    }
}
