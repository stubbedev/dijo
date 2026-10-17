import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.parser.AutoDetectParser;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.html.JSoupParser;
import org.apache.tika.utils.StringUtils;
import org.jsoup.Jsoup;
import org.jsoup.nodes.Attribute;
import org.jsoup.nodes.Comment;
import org.jsoup.nodes.DataNode;
import org.jsoup.nodes.Document;
import org.jsoup.nodes.DocumentType;
import org.jsoup.nodes.Element;
import org.jsoup.nodes.Node;
import org.jsoup.nodes.TextNode;
import org.jsoup.parser.TagSet;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.stream.Stream;

/**
 * Writes what Apache Tika's JSoupParser makes of every input the Rust parser is checked against,
 * and the trees jsoup builds for them.
 *
 * <p>{@code html-reference.tsv.gz} holds, in ParserReference's form with one more column, the
 * CRC-32 and UTF-8 length of the tree jsoup builds (in the form below, parsed as JSoupParser
 * parses it): each file of the given corpora that Tika's default detector, told the file name as
 * extractous tells it, detects as one of JSoupParser's types, parsed through AutoDetectParser as
 * extractous parses a file; synthetic inputs written to files and parsed the same way; and the
 * documents of html5lib's tree construction tests, in UTF-8, handed to JSoupParser directly as
 * text/html. Where the Rust parser builds the same tree, it must write what Tika writes.
 *
 * <p>{@code html5lib-trees.tsv.gz} holds, for each document of html5lib's tree construction
 * tests that does not need scripting, its label, its base64 UTF-8 text and the base64 UTF-8 tree
 * the test expects, in html5lib's form: the oracle of the Rust parser's trees.
 *
 * <p>The form of a tree, in document order: {@code S name namespace[ key=length:value...]\n}
 * where an element (or the document, {@code #document}) starts, its children, then {@code E\n};
 * {@code T} and {@code C} then {@code length:text\n} for text and a comment, where the text of
 * adjacent text, data and CDATA nodes is one; {@code Y name|public|system\n} for a doctype.
 * Lengths count UTF-16 units; a lone surrogate, which a numeric character reference can make, is
 * written as U+FFFD, as Tika's output writes it.
 */
public final class HtmlReference {

    private static final Set<String> HTML_TYPES = Set.of("text/html", "application/xhtml+xml",
            "application/vnd.wap.xhtml+xml", "application/x-asp");

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: HtmlReference <fixture dir>"
                    + " <html5lib tree-construction dir> <label=dir>...");
            System.exit(2);
        }
        final Path fixtures = Path.of(args[0]);
        final List<Html5libTest> html5lib = html5lib(Path.of(args[1]));
        final int roots = 2;
        final TagSet tags = tika(JSoupParser.class);
        final Detector detector = new DefaultDetector();
        final List<ParserReference.Input> inputs = new ArrayList<>(ParserReference.detected(
                detector, HTML_TYPES, Arrays.copyOfRange(args, roots, args.length)));
        inputs.addAll(ParserReference.synthetic(detector, HTML_TYPES, synthetic()));
        for (Html5libTest test : html5lib) {
            inputs.add(new ParserReference.Input(test.label(), null,
                    test.data().getBytes(StandardCharsets.UTF_8), "text/html", true));
        }
        final JSoupParser htmlParser = new JSoupParser(ParserReference.encodingDetector());
        final Parser autoDetect = new AutoDetectParser(detector, htmlParser);
        final List<String> lines = new ArrayList<>();
        for (ParserReference.Input input : inputs) {
            final String columns = ParserReference.columns(input, htmlParser, autoDetect);
            final Map<String, String> metadata = metadata(columns.split("\t")[5]);
            final String charset = metadata.get("Content-Encoding");
            final String decodeAs = metadata.getOrDefault("tk:decoded-charset", charset);
            final Document document = Jsoup.parse(new ByteArrayInputStream(input.bytes()),
                    decodeAs, "", org.jsoup.parser.Parser.htmlParser().tagSet(new TagSet(tags)));
            lines.add(columns + '\t' + ParserReference.digest(tree(document)));
        }
        ParserReference.write(fixtures.resolve("html-reference.tsv.gz"), lines);

        final List<String> trees = new ArrayList<>();
        for (Html5libTest test : html5lib) {
            if (test.expected() != null) {
                trees.add(test.label() + '\t' + base64(test.data()) + '\t'
                        + base64(test.expected()));
            }
        }
        ParserReference.write(fixtures.resolve("html5lib-trees.tsv.gz"), trees);
        System.err.println("HtmlReference: " + inputs.size() + " inputs, " + trees.size()
                + " html5lib trees");
    }

    private static String base64(String text) {
        return Base64.getEncoder().encodeToString(text.getBytes(StandardCharsets.UTF_8));
    }

    /** The tag set JSoupParser parses with: jsoup's, with Tika's self-closing tags. */
    private static TagSet tika(Class<?> parser) throws ReflectiveOperationException {
        final java.lang.reflect.Field field = parser.getDeclaredField("SELF_CLOSEABLE_TAGS");
        field.setAccessible(true);
        return (TagSet) field.get(null);
    }

    private static Map<String, String> metadata(String column) {
        final Map<String, String> values = new TreeMap<>();
        for (String entry : column.split("\u001f")) {
            if (entry.isEmpty()) {
                continue;
            }
            final String[] pair = entry.split("=", 2);
            values.putIfAbsent(pair[0], pair[1]);
        }
        return values;
    }

    static String tree(Document document) {
        final StringBuilder out = new StringBuilder();
        node(document, out);
        return out.toString();
    }

    /** The text of a text, data or CDATA node, or null for another node. */
    private static String text(Node node) {
        if (node instanceof TextNode text) {
            return text.getWholeText();
        }
        if (node instanceof DataNode data) {
            return data.getWholeData();
        }
        return null;
    }

    private static void node(Node node, StringBuilder out) {
        if (node instanceof Comment comment) {
            text('C', comment.getData(), out);
        } else if (node instanceof DocumentType doctype) {
            out.append("Y ").append(StringUtils.wellFormed(doctype.name())).append('|')
                    .append(StringUtils.wellFormed(doctype.publicId())).append('|')
                    .append(StringUtils.wellFormed(doctype.systemId())).append('\n');
        } else if (node instanceof Element element) {
            out.append("S ").append(StringUtils.wellFormed(element.nodeName())).append(' ')
                    .append(namespace(element.tag().namespace()));
            for (Attribute attribute : element.attributes()) {
                final String value = StringUtils.wellFormed(attribute.getValue());
                out.append(' ').append(StringUtils.wellFormed(attribute.getKey())).append('=')
                        .append(value.length()).append(':').append(value);
            }
            out.append('\n');
            final StringBuilder text = new StringBuilder();
            for (Node child : element.childNodes()) {
                final String childText = text(child);
                if (childText != null) {
                    text.append(childText);
                    continue;
                }
                if (!text.isEmpty()) {
                    text('T', text.toString(), out);
                    text.setLength(0);
                }
                node(child, out);
            }
            if (!text.isEmpty()) {
                text('T', text.toString(), out);
            }
            out.append("E\n");
        } else {
            throw new IllegalStateException("unexpected node " + node.nodeName());
        }
    }

    private static void text(char kind, String text, StringBuilder out) {
        final String wellFormed = StringUtils.wellFormed(text);
        out.append(kind).append(' ').append(wellFormed.length()).append(':').append(wellFormed)
                .append('\n');
    }

    private static String namespace(String namespace) {
        return switch (namespace) {
            case "http://www.w3.org/1999/xhtml" -> "html";
            case "http://www.w3.org/2000/svg" -> "svg";
            case "http://www.w3.org/1998/Math/MathML" -> "math";
            default -> throw new IllegalStateException("unexpected namespace " + namespace);
        };
    }

    /**
     * A document of html5lib's tree construction tests, and the tree it expects without
     * scripting, or null where it expects one only with scripting.
     */
    private record Html5libTest(String label, String data, String expected) {
    }

    /** The documents (not fragments) of html5lib's tree construction tests. */
    private static List<Html5libTest> html5lib(Path dir) throws IOException {
        final List<Html5libTest> tests = new ArrayList<>();
        try (Stream<Path> files = Files.list(dir)) {
            for (Path file : files.filter(f -> f.toString().endsWith(".dat")).sorted().toList()) {
                final List<String> lines =
                        List.of(Files.readString(file, StandardCharsets.UTF_8).split("\n", -1));
                int number = 0;
                for (int i = 0; i < lines.size(); i++) {
                    if (!lines.get(i).equals("#data")) {
                        continue;
                    }
                    number++;
                    final StringBuilder data = new StringBuilder();
                    int j = i + 1;
                    while (j < lines.size() && !lines.get(j).equals("#errors")) {
                        if (j > i + 1) {
                            data.append('\n');
                        }
                        data.append(lines.get(j));
                        j++;
                    }
                    boolean fragment = false;
                    boolean scripting = false;
                    final List<String> expected = new ArrayList<>();
                    boolean inDocument = false;
                    for (int k = j; k < lines.size() && !lines.get(k).equals("#data"); k++) {
                        fragment |= lines.get(k).equals("#document-fragment");
                        scripting |= lines.get(k).equals("#script-on");
                        if (inDocument) {
                            expected.add(lines.get(k));
                        }
                        inDocument |= lines.get(k).equals("#document");
                    }
                    while (!expected.isEmpty() && expected.get(expected.size() - 1).isEmpty()) {
                        expected.remove(expected.size() - 1);
                    }
                    if (!fragment) {
                        tests.add(new Html5libTest("html5lib/" + file.getFileName() + "#" + number,
                                data.toString(), scripting ? null : String.join("\n", expected)));
                    }
                }
            }
        }
        return tests;
    }

    /** Inputs written to files, by file name, which steers Tika's type detection. */
    private static Map<String, byte[]> synthetic() {
        final TreeMap<String, byte[]> inputs = new TreeMap<>();
        final String prose = "Rødgrød med fløde på én skål, æbleskiver og smørrebrød i Århus. ";
        inputs.put("metadata.html", ParserReference.bytes("<!DOCTYPE html>\n<html lang=\"da\"><head>"
                + "<meta charset=\"utf-8\"><meta name=\"author\" content=\"Anne\">"
                + "<meta name=\"Author\" content=\"Bo\"><meta name=\"keywords\" content=\"a, b\">"
                + "<meta name=\"description\" content=\"One\"><meta name=\"description\" content=\"Two\">"
                + "<meta name=\"title\" content=\"Meta title\"><meta name=\"subject\" content=\"S\">"
                + "<meta name=\"ICBM\" content=\" 55.68, 12.57 \"><meta name=\"geo.icbm\" content=\"x\">"
                + "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">"
                + "<meta property=\"og:title\" content=\"Graph\"><meta name=\"empty\" content=\"\">"
                + "<meta name=\" \" content=\"blank name\"><meta content=\"no name\">"
                + "<meta name=\"generator\" content=\"x\"><meta name=\"generator\" content=\"y\">"
                + "<title>  The &amp; title  </title><meta name=\"title\" content=\"Late\">"
                + "<link rel=\"stylesheet\" href=\" style.css \" media=\"all\" title=\"t\">"
                + "<style>p { color: red }</style><script>var x = '<p>';</script></head>\n"
                + "<body><h1>Heading</h1><p>" + prose + "</p></body></html>\n", "UTF-8"));
        inputs.put("icbm-invalid.html", ParserReference.bytes("<html><head><meta name=\"icbm\" "
                + "content=\"north\"><meta http-equiv=\"content-type\" content=\"nonsense\">"
                + "<title>a</title><title>b</title></head><body>x</body></html>", "UTF-8"));
        inputs.put("links.html", ParserReference.bytes("<html><body>"
                + "<a href=\" http://example.com/a \" title=\"t\" name=\"n\" rel=\"r\">link</a> "
                + "<a>no href</a><img src=\"i.png\"><img src=\"j.png\" alt=\"J\" class=\"c\">"
                + "<map name=\"m\" id=\"m1\"><area shape=\"rect\" coords=\"0,0,1,1\" href=\"#a\"></map>"
                + "<object data=\"movie.swf\" type=\"x\" classid=\"c\"><param name=\"p\" value=\"v\">"
                + "fallback</object><iframe src=\"f.html\" width=\"10\">frame <b>text</b></iframe>"
                + "<q cite=\"c\">quote</q><blockquote cite=\" b \">block</blockquote>"
                + "<ins datetime=\"d\" cite=\"x\">ins</ins><del>del</del>"
                + "<a href=\"mailto: me@example.com\">mail</a><a href=\"javascript:void(0)\">js</a>"
                + "</body></html>", "UTF-8"));
        inputs.put("base-relative.html", ParserReference.bytes("<html><head><base href=\" docs/ \">"
                + "</head><body><a href=\"a.html\">a</a><img src=\"b.png\"></body></html>",
                "UTF-8"));
        inputs.put("structure.html", ParserReference.bytes("<html><body><div>one<div>two</div>"
                + "</div><section>sec</section><ul><li>a<li>b</ul><ol><li>c</ol>"
                + "<menu><li>m</menu><dl><dt>t<dd>d</dl><pre>\npre\r\ntext</pre>"
                + "<table><caption>cap</caption><tr><th>h<td>c<td>d</table>"
                + "<table>foster<tr><td>x</td></tr>text</table>"
                + "<table><tbody><tr><td><table><tr><td>nested</table></table>"
                + "<address>addr</address><span>span</span><br>after<hr>rule"
                + "<noscript><p>no script</p></noscript><textarea>t &amp; a</textarea>"
                + "<select><option>o1<option>o2</select><button>b</button>"
                + "<form><input type=\"text\" value=\"v\"><label>l</label></form>"
                + "<!-- comment --><![CDATA[cdata in html]]><plaintext><b>plain</b>"
                + "</body></html>", "UTF-8"));
        inputs.put("misnested.html", ParserReference.bytes("<html><body><b>bold<p>para</b>rest</p>"
                + "<a href=\"x\">one<a href=\"y\">two</a><i><b>ib</i>b</b>"
                + "<h1>h<h2>h2</h1><p>p<table><tr><td>cell</td></tr></table>"
                + "<div/>self<p/>closed<span/>span<a/>anchor<td/>cell<custom/>custom"
                + "<image src=\"img.png\"><li>stray</body></html>", "UTF-8"));
        inputs.put("foreign.html", ParserReference.bytes("<html><body><svg viewBox=\"0 0 1 1\">"
                + "<title>svg title</title><a href=\"svg-link\">svg a</a><text>words</text>"
                + "<foreignObject><p>html in svg</p></foreignObject><script>s</script>"
                + "<![CDATA[svg cdata]]><path d=\"M0\"/></svg><math><mi>x</mi><mtext>t</mtext>"
                + "<annotation-xml encoding=\"text/html\"><p>ann</p></annotation-xml></math>"
                + "<p>after</p></body></html>", "UTF-8"));
        inputs.put("entities.html", ParserReference.bytes("<html><body><p>&amp; &lt; &notit; &noti; "
                + "&notin; &#x80; &#150; &#0; &#xD800; &#1114112; &#x10FFFF; &AElig &aelig; "
                + "&amp &unknown; &#; &#x; &NotNestedLessLess; &acE;</p>"
                + "<a href=\"?a=1&amp;b=2&copy=3&notin\">attr</a></body></html>", "UTF-8"));
        inputs.put("frames.html", ParserReference.bytes("<html><head><title>Frames</title></head>"
                + "<frameset cols=\"50%,50%\"><frame src=\"a.html\" name=\"a\"><frame src=\"b.html\">"
                + "<noframes><body>no frames</body></noframes></frameset></html>", "UTF-8"));
        inputs.put("windows-1252.html", ParserReference.bytes("<html><head><meta http-equiv="
                + "\"Content-Type\" content=\"text/html; charset=windows-1252\"><title>Æble</title>"
                + "</head><body><p>" + prose + " €</p></body></html>", "windows-1252"));
        inputs.put("utf8-bom.html", ParserReference.concat(ParserReference.hex("ef bb bf"),
                ParserReference.bytes("<html><body><p>" + prose + "</p></body></html>", "UTF-8")));
        inputs.put("utf16-bom.html", ParserReference.concat(ParserReference.hex("ff fe"),
                ParserReference.bytes("<html><body><p>" + prose + "</p></body></html>",
                        "UTF-16LE")));
        inputs.put("meta-mismatch.html", ParserReference.bytes("<html><head><meta charset=\"utf-8\">"
                + "</head><body><p>" + prose + "</p></body></html>", "ISO-8859-1"));
        inputs.put("crlf.html", ParserReference.bytes("<html>\r\n<body>\r\n<p>line one\r\nline two"
                + "\rline three</p>\r\n<pre>\r\nkept</pre>\r\n</body>\r\n</html>\r\n", "UTF-8"));
        inputs.put("ffff.html", ParserReference.bytes("<html><body><p>before \uFFFF after</p>"
                + "</body></html>", "UTF-8"));
        inputs.put("controls.html", ParserReference.bytes("<html><body><p>nul\u0000 bell\u0007 "
                + "c1\u0085 del\u007F</p><p title=\"a\u0001b\">x</p></body></html>", "UTF-8"));
        final StringBuilder large = new StringBuilder("<html><body>");
        for (int i = 0; large.length() < 9000; i++) {
            large.append("<p class=\"c").append(i).append("\">paragraph ").append(i)
                    .append(" &amp; entity &copy; ").append(prose).append("</p>\n");
        }
        inputs.put("large.html", ParserReference.bytes(large + "</body></html>", "UTF-8"));
        final StringBuilder boundary = new StringBuilder("<html><body><p>");
        while (boundary.length() < 2040) {
            boundary.append('x');
        }
        inputs.put("buffer-boundary.html", ParserReference.bytes(boundary
                + "&notin;<b>bold</b>😀&amp;</p><script>" + "y".repeat(2040)
                + "</scr" + "ipt><p>end</p></body></html>", "UTF-8"));
        inputs.put("no-structure.html", ParserReference.bytes("<title>Only</title>text <b>bold",
                "UTF-8"));
        inputs.put("head-body-text.html", ParserReference.bytes("<html>before<head>in head"
                + "<title>T</title></head>between<body>body</body>after</html>trailing",
                "UTF-8"));
        inputs.put("title-in-body.html", ParserReference.bytes("<html><body><p>x</p>"
                + "<title>Body title</title></body></html>", "UTF-8"));
        inputs.put("xhtml.xhtml", ParserReference.bytes("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
                + "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" "
                + "\"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">\n"
                + "<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\"><head>"
                + "<title>XHTML</title></head><body><p>x<br/>y</p></body></html>\n", "UTF-8"));
        inputs.put("data-uri.html", ParserReference.bytes("<html><body><p>image</p>"
                + "<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"a\"></body></html>",
                "UTF-8"));
        inputs.put("srcdoc.html", ParserReference.bytes("<html><body><iframe srcdoc=\"<p>inner</p>\">"
                + "</iframe></body></html>", "UTF-8"));
        return inputs;
    }
}
