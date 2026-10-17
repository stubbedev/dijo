import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.detect.zip.DefaultZipContainerDetector;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;

import java.io.IOException;
import java.io.InputStream;
import java.io.Writer;
import java.lang.reflect.Field;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Collectors;
import java.util.stream.Stream;

/**
 * Writes what extractous's Tika detector answers for every document of the parity corpora: the
 * media type and the metadata the detection adds, in the order it adds them, once for the file
 * (as TikaNativeMain.detectFile opens it, with its name and length) and once for its bytes alone
 * (as TikaNativeMain.detectBytes streams them). A line is the corpus, the document's path in
 * it, the variant, the media type, then the metadata the detectors add. What opening the input
 * writes before detection, the file's name and length, is checked and left out.
 *
 * The detector is built exactly as TikaNativeMain builds it, with new DefaultDetector(), so it
 * must run on the native image's class path in its order: the order of the jars decides the
 * order of the ZIP container detectors, which DefaultDetector does not sort.
 */
public final class MimeReference {

    private static final List<String> DETECTORS = List.of(
            "org.apache.tika.detect.apple.BPListDetector",
            "org.apache.tika.detect.gzip.GZipSpecializationDetector",
            "org.apache.tika.detect.image.RawTiffDetector",
            "org.apache.tika.detect.microsoft.POIFSContainerDetector",
            "org.apache.tika.detect.mkv.MatroskaDetector",
            "org.apache.tika.detect.ogg.OggDetector",
            "org.apache.tika.detect.ole.MiscOLEDetector",
            "org.apache.tika.detect.zip.DefaultZipContainerDetector");
    private static final List<String> ZIP_DETECTORS = List.of(
            "org.apache.tika.detect.microsoft.ooxml.OPCPackageDetector",
            "org.apache.tika.detect.apple.IWorkDetector",
            "org.apache.tika.detect.zip.IPADetector",
            "org.apache.tika.detect.zip.JarDetector",
            "org.apache.tika.detect.zip.KMZDetector",
            "org.apache.tika.detect.zip.OpenDocumentDetector",
            "org.apache.tika.detect.zip.StarOfficeDetector",
            "org.apache.tika.detect.zip.FrictionlessPackageDetector",
            "org.apache.tika.detect.zip.GeoGebraDetector");

    private static final Detector DETECTOR = new DefaultDetector();

    /** Metadata that records every value written to it, in order. */
    private static final class RecordingMetadata extends Metadata {
        private final List<String> writes = new ArrayList<>();

        @Override
        public void setTrusted(String name, String value) {
            writes.add(name + "=" + value);
            super.setTrusted(name, value);
        }

        @Override
        public void addTrusted(String name, String value) {
            throw new IllegalStateException("a detector added a second value to " + name);
        }
    }

    /** TikaNativeMain's stream over a byte buffer, rewritten here as the image compiles it. */
    private static final class ByteBufferInputStream extends InputStream {
        private ByteBuffer bb;

        ByteBufferInputStream(ByteBuffer bb) {
            this.bb = bb;
        }

        @Override
        public int read() throws IOException {
            if (bb == null) {
                throw new IOException("read on a closed InputStream");
            }
            if (bb.remaining() == 0) {
                return -1;
            }
            return bb.get() & 0xFF;
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            if (bb == null) {
                throw new IOException("read on a closed InputStream");
            }
            if (b == null) {
                throw new NullPointerException();
            } else if (off < 0 || len < 0 || len > b.length - off) {
                throw new IndexOutOfBoundsException();
            } else if (len == 0) {
                return 0;
            }
            int length = Math.min(bb.remaining(), len);
            if (length == 0) {
                return -1;
            }
            bb.get(b, off, length);
            return length;
        }

        @Override
        public long skip(long n) throws IOException {
            if (bb == null) {
                throw new IOException("skip on a closed InputStream");
            }
            if (n <= 0) {
                return 0;
            }
            int nInt = (int) n;
            int skip = Math.min(bb.remaining(), nInt);
            bb.position(bb.position() + skip);
            return nInt;
        }

        @Override
        public int available() throws IOException {
            if (bb == null) {
                throw new IOException("available on a closed InputStream");
            }
            return bb.remaining();
        }

        @Override
        public void close() {
            bb = null;
        }
    }

    @FunctionalInterface
    private interface Source {
        TikaInputStream open(Metadata metadata) throws IOException;
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 4) {
            System.err.println("usage: MimeReference <test_files/documents> <quality/corpus> "
                    + "<tika test documents> <output>");
            System.exit(2);
        }
        checkComposition();
        try (Writer out = Files.newBufferedWriter(Path.of(args[3]), StandardCharsets.UTF_8)) {
            write(out, "documents", Path.of(args[0]));
            write(out, "quality", Path.of(args[1]));
            write(out, "tika", Path.of(args[2]));
        }
    }

    private static void checkComposition() throws ReflectiveOperationException {
        final List<String> detectors = ((DefaultDetector) DETECTOR).getDetectors().stream()
                .map(d -> d.getClass().getName()).toList();
        if (!detectors.equals(DETECTORS)) {
            throw new IllegalStateException("DefaultDetector composes " + detectors);
        }
        final DefaultZipContainerDetector zip = (DefaultZipContainerDetector)
                ((DefaultDetector) DETECTOR).getDetectors().get(DETECTORS.size() - 1);
        final Field field = DefaultZipContainerDetector.class.getDeclaredField("staticZipDetectors");
        field.setAccessible(true);
        final List<String> zipDetectors = ((List<?>) field.get(zip)).stream()
                .map(d -> d.getClass().getName()).toList();
        if (!zipDetectors.equals(ZIP_DETECTORS)) {
            throw new IllegalStateException("the ZIP container detector composes " + zipDetectors);
        }
    }

    private static void write(Writer out, String corpus, Path root) throws IOException {
        final List<Path> files;
        try (Stream<Path> walk = Files.walk(root)) {
            files = walk.filter(Files::isRegularFile).sorted().toList();
        }
        for (Path file : files) {
            final String name = root.relativize(file).toString();
            if (name.contains("\t") || name.contains("\n")) {
                throw new IllegalStateException("a tab or newline in the name of " + file);
            }
            final byte[] bytes = Files.readAllBytes(file);
            detect(out, corpus, name, "file", metadata -> TikaInputStream.get(file, metadata));
            detect(out, corpus, name, "bytes", metadata -> TikaInputStream.get(
                    new ByteBufferInputStream(ByteBuffer.wrap(bytes)), new TemporaryResources(),
                    metadata));
        }
    }

    private static void detect(Writer out, String corpus, String name, String variant,
                               Source source) throws IOException {
        final RecordingMetadata metadata = new RecordingMetadata();
        final String result;
        try (TikaInputStream stream = source.open(metadata)) {
            final List<String> expected = variant.equals("file")
                    ? List.of("tk:resource-name=" + Path.of(name).getFileName(),
                    "Content-Length=" + stream.getLength())
                    : List.of();
            if (!metadata.writes.equals(expected)) {
                throw new IllegalStateException("opening " + name + " wrote " + metadata.writes);
            }
            metadata.writes.clear();
            result = DETECTOR.detect(stream, metadata, new ParseContext()).toString();
        }
        out.write(corpus + "\t" + name + "\t" + variant + "\t" + result
                + metadata.writes.stream().map(write -> "\t" + write)
                .collect(Collectors.joining()) + "\n");
    }
}
