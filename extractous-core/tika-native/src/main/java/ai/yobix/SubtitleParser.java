package ai.yobix;

import org.apache.tika.detect.AutoDetectReader;
import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.apache.tika.utils.CharsetUtils;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.regex.Pattern;

/**
 * Subtitles (SubRip, WebVTT, SubStation Alpha) as their spoken text, one paragraph per cue:
 * without cue numbers, timings, cue settings, styles, notes or the markup inside cues. Tika
 * passes them through as plain text, timings and tags included.
 */
final class SubtitleParser implements Parser {

    static final MediaType SUBRIP = MediaType.application("x-subrip");
    static final MediaType WEBVTT = MediaType.text("vtt");
    static final MediaType SUBSTATION = MediaType.text("x-ssa");

    private static final Pattern TIMING = Pattern.compile(
            "^\\s*(\\d+:)?\\d{1,2}:\\d{2}[.,]\\d{1,3}\\s*-->\\s*(\\d+:)?\\d{1,2}:\\d{2}[.,]\\d{1,3}.*$");
    private static final Pattern MARKUP = Pattern.compile("<[^>]*>|\\{\\\\[^}]*\\}");
    private static final Pattern ESCAPED_LINE_BREAK = Pattern.compile("\\\\[Nn]");
    private static final String SUBSTATION_DIALOGUE = "Dialogue:";
    private static final int SUBSTATION_TEXT_FIELD = 9;

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(SUBRIP, WEBVTT, SUBSTATION);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final MediaType type = MediaType.parse(metadata.get(HttpHeaders.CONTENT_TYPE));
        try (AutoDetectReader reader = new AutoDetectReader(stream, metadata)) {
            metadata.set(HttpHeaders.CONTENT_ENCODING, CharsetUtils.clean(reader.getCharset().name()));
            final List<String> cues = type != null && type.getBaseType().equals(SUBSTATION)
                    ? substationCues(reader) : timedCues(reader);
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
            xhtml.startDocument();
            for (final String cue : cues) {
                xhtml.element("p", cue);
            }
            xhtml.endDocument();
        }
    }

    /**
     * The text of SubRip and WebVTT cues: the lines after each timing line, up to a blank line.
     * WebVTT headers, notes and style blocks have no timing line, so they are left out.
     */
    static List<String> timedCues(BufferedReader reader) throws IOException {
        final List<String> cues = new ArrayList<>();
        final StringBuilder cue = new StringBuilder();
        boolean inCue = false;
        String line;
        while ((line = reader.readLine()) != null) {
            if (TIMING.matcher(line).matches()) {
                inCue = true;
                continue;
            }
            if (line.isBlank()) {
                addCue(cues, cue);
                inCue = false;
                continue;
            }
            if (!inCue) {
                continue;
            }
            if (!cue.isEmpty()) {
                cue.append('\n');
            }
            cue.append(clean(line));
        }
        addCue(cues, cue);
        return cues;
    }

    /** The text field of each Dialogue line of a SubStation Alpha script. */
    static List<String> substationCues(BufferedReader reader) throws IOException {
        final List<String> cues = new ArrayList<>();
        String line;
        while ((line = reader.readLine()) != null) {
            if (!line.startsWith(SUBSTATION_DIALOGUE)) {
                continue;
            }
            final String[] fields =
                    line.substring(SUBSTATION_DIALOGUE.length()).split(",", SUBSTATION_TEXT_FIELD + 1);
            if (fields.length > SUBSTATION_TEXT_FIELD) {
                final StringBuilder cue = new StringBuilder(
                        clean(ESCAPED_LINE_BREAK.matcher(fields[SUBSTATION_TEXT_FIELD]).replaceAll("\n")));
                addCue(cues, cue);
            }
        }
        return cues;
    }

    private static String clean(String line) {
        return MARKUP.matcher(line).replaceAll("").strip();
    }

    private static void addCue(List<String> cues, StringBuilder cue) {
        final String text = cue.toString().strip();
        if (!text.isEmpty()) {
            cues.add(text);
        }
        cue.setLength(0);
    }
}
