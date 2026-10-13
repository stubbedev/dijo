import org.apache.tika.langdetect.charsoup.CharSoupLanguageDetector;
import org.apache.tika.language.detect.LanguageResult;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.stream.Stream;

/**
 * Writes what Apache Tika's CharSoup language detector answers for every input the port is checked
 * against: each Tatoeba sentence, runs of five sentences of one language, each quality truth text and
 * its paragraphs, and the edge cases. A line is the escaped input, then the language (empty when
 * CharSoup finds none), the winner's score, the entropy and the confidence. Inputs without a letter
 * are left out, since the port answers no language for them where CharSoup answers its biases.
 */
public final class CharSoupReference {

    private static final int RUN = 5;

    public static void main(String[] args) throws IOException {
        if (args.length != 3) {
            System.err.println("usage: CharSoupReference <fixtures dir> <truth dir> <output>");
            System.exit(2);
        }
        final Path fixtures = Path.of(args[0]);
        final List<String> inputs = new ArrayList<>();
        final Map<String, List<String>> byLanguage = new LinkedHashMap<>();
        for (String line : Files.readAllLines(fixtures.resolve("tatoeba.tsv"), StandardCharsets.UTF_8)) {
            final String[] fields = line.split("\t", 3);
            inputs.add(fields[2]);
            byLanguage.computeIfAbsent(fields[0], language -> new ArrayList<>()).add(fields[2]);
        }
        for (List<String> sentences : byLanguage.values()) {
            for (int start = 0; start + RUN <= sentences.size(); start += RUN) {
                inputs.add(String.join(" ", sentences.subList(start, start + RUN)));
            }
        }
        try (Stream<Path> truths = Files.list(Path.of(args[1]))) {
            for (Path truth : truths.sorted().toList()) {
                final String text = Files.readString(truth, StandardCharsets.UTF_8);
                inputs.add(text);
                for (String paragraph : text.split("\n\\s*\n")) {
                    inputs.add(paragraph.strip());
                }
            }
        }
        for (String line : Files.readAllLines(fixtures.resolve("edge-cases.txt"), StandardCharsets.UTF_8)) {
            inputs.add(unescape(line));
        }

        final CharSoupLanguageDetector detector = new CharSoupLanguageDetector();
        final StringBuilder out = new StringBuilder();
        for (String input : inputs) {
            if (input.codePoints().noneMatch(Character::isLetter)) {
                continue;
            }
            detector.reset();
            detector.addText(input);
            final LanguageResult top = detector.detectAll().get(0);
            out.append(escape(input)).append('\t')
                    .append(top.getLanguage()).append('\t')
                    .append(top.getRawScore()).append('\t')
                    .append(detector.getDistributionEntropy()).append('\t')
                    .append(top.getConfidence().name().toLowerCase(Locale.ROOT)).append('\n');
        }
        Files.writeString(Path.of(args[2]), out.toString(), StandardCharsets.UTF_8);
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t").replace("\r", "\\r");
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
                default -> throw new IllegalArgumentException("Unknown escape \\" + next + " in " + line);
            }
        }
        return text.toString();
    }
}
