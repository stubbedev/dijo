package ai.yobix;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.regex.Pattern;

/**
 * Jupyter notebooks as their cells in order: markdown as paragraphs, code and outputs as
 * preformatted text, the text of streams, results and errors without terminal colour codes.
 * Tika reads a notebook as plain JSON text, markup and all. Covers nbformat 4 (cells with a
 * source) and nbformat 3 (worksheets of cells with an input).
 */
final class NotebookParser implements Parser {

    static final MediaType NOTEBOOK = MediaType.application("x-ipynb+json");

    private static final ObjectMapper JSON = new ObjectMapper();
    private static final Pattern TERMINAL_CODES = Pattern.compile("\u001B\\[[0-9;]*[A-Za-z]");
    private static final String PLAIN_TEXT = "text/plain";
    private static final String MARKDOWN = "text/markdown";

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(NOTEBOOK);
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final JsonNode notebook;
        try {
            notebook = JSON.readTree(stream);
        } catch (JsonProcessingException e) {
            throw new TikaException("The notebook is not valid JSON", e);
        }
        if (notebook == null || !notebook.isObject()) {
            throw new TikaException("The notebook is not a JSON object");
        }
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        xhtml.startDocument();
        for (final JsonNode cell : cells(notebook)) {
            final String type = cell.path("cell_type").asText();
            final String source = text(cell.has("source") ? cell.get("source") : cell.get("input"));
            if ("markdown".equals(type) || "raw".equals(type) || "heading".equals(type)) {
                for (final String paragraph : source.split("\n\\s*\n")) {
                    if (!paragraph.isBlank()) {
                        xhtml.element("p", paragraph.strip());
                    }
                }
                continue;
            }
            if (!source.isBlank()) {
                xhtml.element("pre", source);
            }
            for (final JsonNode output : cell.path("outputs")) {
                final String text = outputText(output);
                if (!text.isBlank()) {
                    xhtml.element("pre", text);
                }
            }
        }
        xhtml.endDocument();
    }

    private static List<JsonNode> cells(JsonNode notebook) {
        final List<JsonNode> cells = new ArrayList<>();
        notebook.path("cells").forEach(cells::add);
        for (final JsonNode worksheet : notebook.path("worksheets")) {
            worksheet.path("cells").forEach(cells::add);
        }
        return cells;
    }

    /** The text of an output: a stream, the plain or markdown form of a result, or an error. */
    private static String outputText(JsonNode output) {
        if (output.has("text")) {
            return clean(text(output.get("text")));
        }
        final JsonNode data = output.path("data");
        if (data.has(PLAIN_TEXT)) {
            return clean(text(data.get(PLAIN_TEXT)));
        }
        if (data.has(MARKDOWN)) {
            return clean(text(data.get(MARKDOWN)));
        }
        if (output.has("traceback")) {
            final List<String> lines = new ArrayList<>();
            output.get("traceback").forEach(line -> lines.add(line.asText()));
            return clean(String.join("\n", lines));
        }
        if (output.has("ename")) {
            return clean(output.path("ename").asText() + ": " + output.path("evalue").asText());
        }
        return "";
    }

    /** A notebook string, stored either whole or as an array of lines. */
    private static String text(JsonNode value) {
        if (value == null || value.isNull()) {
            return "";
        }
        if (!value.isArray()) {
            return value.asText();
        }
        final StringBuilder joined = new StringBuilder();
        value.forEach(line -> joined.append(line.asText()));
        return joined.toString();
    }

    private static String clean(String text) {
        return TERMINAL_CODES.matcher(text).replaceAll("").strip();
    }
}
