import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.zip.ZipFile;

import javax.tools.JavaCompiler;
import javax.tools.ToolProvider;

/**
 * Compiles Tika's StructuralEncodingRules with the change the extractous native image
 * substitutes into it (ai.yobix.Target_org_apache_tika_ml_chardetect_StructuralEncodingRules):
 * UTF-16 column asymmetry counts as evidence only when the bytes hold a NUL. Put ahead of Tika's
 * jars on a class path, the class makes the JVM detect encodings as the native image does, which
 * the reference generators need. It reads the class's source from Tika's pinned sources jar and
 * fails if the method is not exactly as Tika 4.1.0 wrote it.
 */
public final class NativeImageRules {

    private static final String SOURCE = "org/apache/tika/ml/chardetect/StructuralEncodingRules.java";
    private static final String TIKA = """
                public static boolean has2ByteColumnAsymmetryEvidence(byte[] bytes) {
                    if (bytes == null || bytes.length < MIN_COLUMN_EVIDENCE_PROBE) {
                        return false;
                    }
                    return computeColumnAsymmetry(bytes);
                }
            """;
    private static final String NATIVE_IMAGE = """
                public static boolean has2ByteColumnAsymmetryEvidence(byte[] bytes) {
                    if (bytes == null || bytes.length < MIN_COLUMN_EVIDENCE_PROBE) {
                        return false;
                    }
                    for (byte value : bytes) {
                        if (value == 0) {
                            return computeColumnAsymmetry(bytes);
                        }
                    }
                    return false;
                }
            """;

    public static void main(String[] args) throws IOException {
        if (args.length != 3) {
            System.err.println("usage: NativeImageRules <sources jar> <class path> <output dir>");
            System.exit(2);
        }
        final String source;
        try (ZipFile jar = new ZipFile(args[0]);
             InputStream in = jar.getInputStream(jar.getEntry(SOURCE))) {
            source = new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
        final int at = source.indexOf(TIKA);
        if (at < 0 || source.indexOf(TIKA, at + 1) >= 0) {
            throw new IllegalStateException(SOURCE + " does not hold the method once as Tika 4.1.0 wrote it");
        }
        final Path work = Files.createTempDirectory("native-image-rules");
        final Path patched = work.resolve("StructuralEncodingRules.java");
        Files.writeString(patched, source.replace(TIKA, NATIVE_IMAGE));
        final JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        final int status = compiler.run(null, null, null, "-nowarn", "-cp", args[1], "-d", args[2],
                patched.toString());
        if (status != 0) {
            throw new IllegalStateException("javac failed on the patched " + SOURCE);
        }
    }
}
