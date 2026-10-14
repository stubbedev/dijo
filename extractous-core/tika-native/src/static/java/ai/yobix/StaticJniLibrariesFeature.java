package ai.yobix;

import com.oracle.svm.core.jdk.NativeLibrarySupport;
import com.oracle.svm.core.jdk.PlatformNativeLibrarySupport;
import com.oracle.svm.hosted.c.NativeLibraries;
import org.graalvm.nativeimage.hosted.Feature;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;

/**
 * Links the JNI libraries Tika needs into a statically linked image, as GraalVM links libjava and
 * libnio, since a static executable cannot load shared libraries: the JDK's headless AWT
 * libraries, which GraalVM ships as static archives but on Linux only links as shared libraries,
 * and sqlite-jdbc's, which ci/static-sqlite.sh compiles. Each AWT archive is registered as a
 * built-in JNI library, and exactly the JNI functions it defines are bound to it, so that the
 * natives of the headful X11 toolkit, which no archive here defines, stay unbound. GraalVM only
 * runs the load hooks of JDK libraries, so sqlite-jdbc's is linked as a plain library whose load
 * hook its loader runs ({@link Target_org_sqlite_SQLiteJDBCLoader}). The natives of
 * {@code LinkedTesseract} are bound to tesseract.cc, linked with libtesseract, and those of
 * {@code LinkedImages} to images.cc, linked with libheif.
 */
public final class StaticJniLibrariesFeature implements Feature {

    private static final String AWT = "awt";
    private static final String MEDIALIB = "mlib_image";
    private static final String[] LIBRARIES = {AWT, "awt_headless", "fontmanager", "javajpeg", "lcms"};
    private static final Map<String, String> PLATFORMS = Map.of("amd64", "linux-amd64", "aarch64", "linux-aarch64");
    private static final String JNI_PREFIX = "Java_";
    private static final String SQLITE = "sqlitejdbc";
    private static final String SQLITE_NATIVES = "org_sqlite_core_NativeDB";
    private static final String TESSERACT_NATIVES = "ai_yobix_LinkedTesseract";
    private static final String IMAGES_NATIVES = "ai_yobix_LinkedImages";

    /**
     * Binds the native methods whose JNI symbols start with a prefix to the linked code: GraalVM
     * 25.2 calls it addBuiltinPkgNativePrefix, 25.4 addBuiltinNativePrefix.
     */
    private static void bindNatives(String prefix) {
        for (final String name : new String[]{"addBuiltinNativePrefix", "addBuiltinPkgNativePrefix"}) {
            final Method method;
            try {
                method = PlatformNativeLibrarySupport.class.getMethod(name, String.class);
            } catch (NoSuchMethodException e) {
                continue;
            }
            try {
                method.invoke(PlatformNativeLibrarySupport.singleton(), prefix);
                return;
            } catch (IllegalAccessException | InvocationTargetException e) {
                throw new IllegalStateException("Cannot bind the natives of " + prefix, e);
            }
        }
        throw new IllegalStateException("This GraalVM has no way to bind natives to linked code");
    }

    @Override
    public void beforeAnalysis(BeforeAnalysisAccess access) {
        final String arch = System.getProperty("os.arch");
        final String platform = PLATFORMS.get(arch);
        if (platform == null) {
            throw new IllegalStateException("No static AWT libraries for the architecture " + arch);
        }
        final Path dir = Path.of(System.getProperty("java.home"), "lib", "static", platform, "musl");
        NativeLibraries.singleton().addStaticNonJniLibrary(MEDIALIB);
        NativeLibraries.singleton().addStaticNonJniLibrary(SQLITE);
        bindNatives(SQLITE_NATIVES);
        bindNatives(TESSERACT_NATIVES);
        bindNatives(IMAGES_NATIVES);
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
                        bindNatives(symbol.substring(JNI_PREFIX.length()));
                    }
                }
            } catch (IOException e) {
                throw new UncheckedIOException("Cannot read " + symbols, e);
            }
        }
    }
}
