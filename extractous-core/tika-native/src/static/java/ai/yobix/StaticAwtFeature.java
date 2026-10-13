package ai.yobix;

import com.oracle.svm.core.jdk.NativeLibrarySupport;
import com.oracle.svm.core.jdk.PlatformNativeLibrarySupport;
import com.oracle.svm.hosted.c.NativeLibraries;
import org.graalvm.nativeimage.hosted.Feature;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;

/**
 * Links the JDK's headless AWT libraries into a statically linked image, as GraalVM links libjava
 * and libnio: GraalVM ships them as static archives but on Linux only links them as shared
 * libraries, which a static executable cannot load. Each archive is registered as a built-in JNI
 * library, and exactly the JNI functions it defines are bound to it, so that the natives of the
 * headful X11 toolkit, which no archive here defines, stay unbound.
 */
public final class StaticAwtFeature implements Feature {

    private static final String AWT = "awt";
    private static final String MEDIALIB = "mlib_image";
    private static final String[] LIBRARIES = {AWT, "awt_headless", "fontmanager", "javajpeg", "lcms"};
    private static final Map<String, String> PLATFORMS = Map.of("amd64", "linux-amd64", "aarch64", "linux-aarch64");
    private static final String JNI_PREFIX = "Java_";

    @Override
    public void beforeAnalysis(BeforeAnalysisAccess access) {
        final String arch = System.getProperty("os.arch");
        final String platform = PLATFORMS.get(arch);
        if (platform == null) {
            throw new IllegalStateException("No static AWT libraries for the architecture " + arch);
        }
        final Path dir = Path.of(System.getProperty("java.home"), "lib", "static", platform, "musl");
        NativeLibraries.singleton().addStaticNonJniLibrary(MEDIALIB);
        for (final String library : LIBRARIES) {
            NativeLibrarySupport.singleton().preregisterUninitializedBuiltinLibrary(library);
            if (library.equals(AWT)) {
                NativeLibraries.singleton().addStaticJniLibrary(library, MEDIALIB);
            } else {
                NativeLibraries.singleton().addStaticJniLibrary(library, AWT);
            }
            final Path symbols = dir.resolve("lib" + library + ".a.symbols");
            try {
                for (final String symbol : Files.readAllLines(symbols)) {
                    if (symbol.startsWith(JNI_PREFIX)) {
                        PlatformNativeLibrarySupport.singleton()
                                .addBuiltinPkgNativePrefix(symbol.substring(JNI_PREFIX.length()));
                    }
                }
            } catch (IOException e) {
                throw new UncheckedIOException("Cannot read " + symbols, e);
            }
        }
    }
}
