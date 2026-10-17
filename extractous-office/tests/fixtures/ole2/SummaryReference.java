import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.stream.Stream;
import java.util.zip.GZIPOutputStream;

import org.apache.poi.poifs.filesystem.POIFSFileSystem;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.microsoft.SummaryExtractor;

/**
 * Writes what Tika's {@link SummaryExtractor} makes of the summary information of OLE2
 * compound documents, for the Rust port of HPSF and of the extractor to be tested against:
 * {@code summaries.tsv.gz}, for each document a line {@code #<key>}, then a line
 * {@code <name>\t<value>} for each value in the order of the sorted names, or a line
 * {@code !unreadable} where POI does not open the document, {@code !thrown} where the extractor
 * throws.
 * <p>
 * A document's key is the name of the root it was found under, a slash and its path below that
 * root, with slashes. Backslashes, tabs and line breaks in names and values are escaped.
 * <p>
 * Usage: {@code java -cp <POI, Tika's microsoft module and their dependencies>
 * SummaryReference.java <output> (<name>=<root>)...}.
 */
public class SummaryReference {

    private static final byte[] SIGNATURE = {
        (byte) 0xD0, (byte) 0xCF, 0x11, (byte) 0xE0, (byte) 0xA1, (byte) 0xB1, 0x1A, (byte) 0xE1,
    };

    public static void main(String[] args) throws IOException {
        try (PrintWriter out = new PrintWriter(new OutputStreamWriter(
                new GZIPOutputStream(new FileOutputStream(args[0])), StandardCharsets.UTF_8))) {
            for (String root : Arrays.copyOfRange(args, 1, args.length)) {
                String[] named = root.split("=", 2);
                for (Path file : documents(Path.of(named[1]))) {
                    String relative = Path.of(named[1]).relativize(file).toString()
                            .replace('\\', '/');
                    out.print("#" + named[0] + "/" + relative + "\n");
                    write(file, out);
                }
            }
        }
    }

    private static List<Path> documents(Path root) throws IOException {
        List<Path> documents = new ArrayList<>();
        try (Stream<Path> files = Files.walk(root)) {
            for (Path file : files.filter(Files::isRegularFile).sorted().toList()) {
                if (isOle(file)) {
                    documents.add(file);
                }
            }
        }
        return documents;
    }

    private static boolean isOle(Path file) throws IOException {
        try (InputStream in = Files.newInputStream(file)) {
            return Arrays.equals(in.readNBytes(SIGNATURE.length), SIGNATURE);
        }
    }

    private static void write(Path file, PrintWriter out) {
        Metadata metadata = new Metadata();
        try (POIFSFileSystem fs = new POIFSFileSystem(file.toFile(), true)) {
            try {
                new SummaryExtractor(metadata).parseSummaries(fs.getRoot());
            } catch (TikaException | RuntimeException e) {
                out.print("!thrown\n");
                return;
            }
        } catch (IOException | RuntimeException e) {
            out.print("!unreadable\n");
            return;
        }
        String[] names = metadata.names();
        Arrays.sort(names);
        for (String name : names) {
            for (String value : metadata.getValues(name)) {
                out.print(escape(name) + "\t" + escape(value) + "\n");
            }
        }
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
                .replace("\r", "\\r");
    }
}
