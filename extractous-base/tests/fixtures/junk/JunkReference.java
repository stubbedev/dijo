import org.apache.tika.ml.junkdetect.JunkDetector;

import java.io.IOException;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.stream.Stream;

/**
 * Writes what Apache Tika's JunkDetector scores for every input the Rust port is checked against:
 * each quality truth text and its paragraphs, each Tatoeba sentence, each paragraph turned into
 * mojibake (its UTF-8 read in a legacy charset, or read as UTF-8 from a legacy charset), reversed
 * and upper-cased, and the edge cases. A line is the escaped input, then the logit (empty for an
 * input Tika cannot score), then the feature z-scores z1 to z9.
 */
public final class JunkReference {

    private static final List<String> MISREAD_AS = List.of("windows-1252", "windows-1251",
            "windows-1250", "ISO-8859-7", "KOI8-R", "Shift_JIS", "GB18030", "x-MacRoman");
    private static final List<String> WRITTEN_IN = List.of("windows-1252", "windows-1250",
            "windows-1251", "ISO-8859-7");

    public static void main(String[] args) throws IOException {
        if (args.length != 4) {
            System.err.println("usage: JunkReference <fixtures dir> <tatoeba tsv> <truth dir> <output>");
            System.exit(2);
        }
        final List<String> inputs = new ArrayList<>();
        try (Stream<Path> truths = Files.list(Path.of(args[2]))) {
            for (Path truth : truths.sorted().toList()) {
                final String text = Files.readString(truth, StandardCharsets.UTF_8);
                inputs.add(text);
                for (String paragraph : text.split("\n\\s*\n")) {
                    final String stripped = paragraph.strip();
                    inputs.add(stripped);
                    inputs.add(new StringBuilder(stripped).reverse().toString());
                    inputs.add(stripped.toUpperCase(Locale.ROOT));
                    final byte[] utf8 = stripped.getBytes(StandardCharsets.UTF_8);
                    for (String charset : MISREAD_AS) {
                        inputs.add(new String(utf8, Charset.forName(charset)));
                    }
                    for (String charset : WRITTEN_IN) {
                        final Charset legacy = Charset.forName(charset);
                        if (legacy.newEncoder().canEncode(stripped)) {
                            inputs.add(new String(stripped.getBytes(legacy), StandardCharsets.UTF_8));
                        }
                    }
                }
            }
        }
        for (String line : Files.readAllLines(Path.of(args[1]), StandardCharsets.UTF_8)) {
            inputs.add(line.split("\t", 3)[2]);
        }
        for (String line : Files.readAllLines(Path.of(args[0]).resolve("edge-cases.txt"),
                StandardCharsets.UTF_8)) {
            inputs.add(unescape(line));
        }

        final JunkDetector detector = JunkDetector.loadFromClasspath();
        final StringBuilder out = new StringBuilder();
        for (String input : inputs) {
            out.append(escape(input));
            if (input.isEmpty()) {
                out.append("\t\n");
                continue;
            }
            final JunkDetector.FeatureComponents parts = detector.scoreWithFeatureComponents(input);
            out.append('\t').append(detector.score(input).getZScore());
            for (float z : new float[]{parts.z1, parts.z2, parts.z3, parts.z4, parts.z5,
                    parts.z6, parts.z7, parts.z8, parts.z9}) {
                out.append('\t').append(z);
            }
            out.append('\n');
        }
        Files.writeString(Path.of(args[3]), out.toString(), StandardCharsets.UTF_8);
    }

    private static String escape(String text) {
        final StringBuilder escaped = new StringBuilder(text.length());
        text.codePoints().forEach(cp -> {
            switch (cp) {
                case '\\' -> escaped.append("\\\\");
                case '\n' -> escaped.append("\\n");
                case '\t' -> escaped.append("\\t");
                case '\r' -> escaped.append("\\r");
                default -> {
                    if (cp < 0x20 || cp == 0x7F || (cp >= 0x80 && cp < 0xA0)) {
                        escaped.append(String.format(Locale.ROOT, "\\u%04x", cp));
                    } else {
                        escaped.appendCodePoint(cp);
                    }
                }
            }
        });
        return escaped.toString();
    }

    private static String unescape(String line) {
        final StringBuilder text = new StringBuilder(line.length());
        for (int index = 0; index < line.length(); index++) {
            final char c = line.charAt(index);
            if (c != '\\') {
                text.append(c);
                continue;
            }
            final char next = line.charAt(++index);
            switch (next) {
                case 'n' -> text.append('\n');
                case 't' -> text.append('\t');
                case 'r' -> text.append('\r');
                case '\\' -> text.append('\\');
                case 'u' -> {
                    text.appendCodePoint(Integer.parseInt(line.substring(index + 1, index + 5), 16));
                    index += 4;
                }
                default -> throw new IllegalArgumentException("Unknown escape \\" + next + " in " + line);
            }
        }
        return text.toString();
    }
}
