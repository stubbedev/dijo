import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

import org.apache.fontbox.cmap.CMap;
import org.apache.fontbox.cmap.CMapParser;
import org.apache.pdfbox.io.RandomAccessReadBuffer;

/**
 * Writes the code FontBox's {@code CMap.getCodesFromUnicode} gives each text that a CMap bundled
 * with FontBox maps more than one one- or two-byte code to, in a map that does nothing but use
 * the bundled one: {@code useCmap} adds the codes in the order of the bundled map's hash maps,
 * so the last of them in that order is the one kept.
 * <p>
 * Usage: {@code java -cp <fontbox jars> UsedCMapCodes.java <fontbox jar> <output.tsv>}
 */
public final class UsedCMapCodes {

    private static final String PREFIX = "org/apache/fontbox/cmap/";

    public static void main(String[] args) throws IOException {
        List<String> names = new ArrayList<>();
        try (ZipFile jar = new ZipFile(args[0])) {
            jar.stream().map(ZipEntry::getName)
                    .filter(name -> name.startsWith(PREFIX) && !name.endsWith("/")
                            && !name.endsWith(".class"))
                    .map(name -> name.substring(PREFIX.length()))
                    .sorted()
                    .forEach(names::add);
        }
        HexFormat hex = HexFormat.of();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[1]),
                StandardCharsets.UTF_8))) {
            out.print("# cmap\tunicode (UTF-16BE hex)\tcode (hex)\n");
            for (String name : names) {
                CMap used = parse("/" + name + " usecmap\n");
                if (!used.hasUnicodeMappings()) {
                    continue;
                }
                Map<String, Integer> codeCounts = new TreeMap<>();
                for (int length = 1; length <= 2; length++) {
                    for (int code = 0; code < 1 << (8 * length); code++) {
                        String unicode = used.toUnicode(code, length);
                        if (unicode != null) {
                            codeCounts.merge(unicode, 1, Integer::sum);
                        }
                    }
                }
                for (Map.Entry<String, Integer> entry : codeCounts.entrySet()) {
                    if (entry.getValue() < 2) {
                        continue;
                    }
                    byte[] codes = used.getCodesFromUnicode(entry.getKey());
                    out.print(name + "\t"
                            + hex.formatHex(entry.getKey().getBytes(StandardCharsets.UTF_16BE))
                            + "\t" + hex.formatHex(codes) + "\n");
                }
            }
        }
    }

    private static CMap parse(String text) throws IOException {
        return new CMapParser().parse(
                new RandomAccessReadBuffer(text.getBytes(StandardCharsets.US_ASCII)));
    }
}
