import java.io.IOException;
import java.io.InputStream;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.stream.Stream;

import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.writelimiter.MetadataWriteLimiter;
import org.apache.tika.parser.ParseContext;

/**
 * Records what Tika's {@code new DefaultDetector()} answers for every file of each corpus, as
 * dijo-mime/tests/fixtures/mime/mime-reference.tsv holds it: one line for the file opened by its
 * path, whose name and length the metadata holds before the detection, and one for its bytes
 * streamed without a name, whose length the metadata learns only if a detector reads them all.
 * Each line is the corpus, the file's path in it with {@code /} between names, the variant
 * ({@code file} or {@code bytes}), the media type, then each metadata value the detection
 * wrote, as {@code name=value}, in the order it first wrote them.
 *
 * <p>Usage: {@code java -cp <tika-app jar and its lib> MimeReference.java OUTPUT
 * CORPUS=DIRECTORY...}
 */
public class MimeReference {

    /** The writes of a detection: the names it wrote, in the order it first wrote them. */
    private static final class Recording implements MetadataWriteLimiter {
        private static final long serialVersionUID = 1L;
        private final Set<String> written = new LinkedHashSet<>();
        private boolean recording;

        @Override
        public void add(String field, String value, Map<String, String[]> data) {
            String[] values = data.get(field);
            if (values == null) {
                set(field, value, data);
                return;
            }
            if (value == null) {
                return;
            }
            String[] more = new String[values.length + 1];
            System.arraycopy(values, 0, more, 0, values.length);
            more[values.length] = value;
            data.put(field, more);
            record(field);
        }

        @Override
        public void set(String field, String value, Map<String, String[]> data) {
            if (value == null) {
                data.remove(field);
            } else {
                data.put(field, new String[] {value});
            }
            record(field);
        }

        private void record(String field) {
            if (recording) {
                written.add(field);
            }
        }
    }

    public static void main(String[] args) throws IOException {
        if (args.length < 2) {
            System.err.println("usage: MimeReference OUTPUT CORPUS=DIRECTORY...");
            System.exit(2);
        }
        Detector detector = new DefaultDetector();
        try (Writer out = Files.newBufferedWriter(Path.of(args[0]), StandardCharsets.UTF_8)) {
            for (int i = 1; i < args.length; i++) {
                String[] corpus = args[i].split("=", 2);
                Path root = Path.of(corpus[1]);
                for (Path file : files(root)) {
                    String name = relative(root, file);
                    for (String variant : new String[] {"file", "bytes"}) {
                        out.write(corpus[0] + "\t" + name + "\t" + variant + "\t"
                                + detect(detector, file, variant.equals("file")) + "\n");
                    }
                }
            }
        }
    }

    private static List<Path> files(Path root) throws IOException {
        try (Stream<Path> walk = Files.walk(root)) {
            return walk.filter(Files::isRegularFile)
                    .sorted((a, b) -> relative(root, a).compareTo(relative(root, b)))
                    .toList();
        }
    }

    private static String relative(Path root, Path file) {
        List<String> names = new ArrayList<>();
        for (Path name : root.relativize(file)) {
            names.add(name.toString());
        }
        return String.join("/", names);
    }

    /** The answer for {@code file}, opened by its path or streamed as bytes. */
    private static String detect(Detector detector, Path file, boolean byPath) {
        Recording recording = new Recording();
        Metadata metadata = new Metadata(recording);
        try (TemporaryResources tmp = new TemporaryResources();
             InputStream bytes = byPath ? null : Files.newInputStream(file);
             TikaInputStream tis = byPath
                     ? TikaInputStream.get(file, metadata, tmp)
                     : TikaInputStream.get(bytes, tmp, metadata)) {
            Map<String, String> before = new HashMap<>();
            for (String name : metadata.names()) {
                before.put(name, metadata.get(name));
            }
            recording.recording = true;
            String type = detector.detect(tis, metadata, new ParseContext()).toString();
            recording.recording = false;
            StringBuilder line = new StringBuilder(type);
            for (String name : recording.written) {
                String value = metadata.get(name);
                if (value != null && !value.equals(before.get(name))) {
                    line.append('\t').append(name).append('=').append(value);
                }
            }
            return line.toString();
        } catch (Exception e) {
            return "error: " + e;
        }
    }
}
