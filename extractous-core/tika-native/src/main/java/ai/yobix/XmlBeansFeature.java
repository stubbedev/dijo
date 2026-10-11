package ai.yobix;

import org.graalvm.nativeimage.hosted.Feature;
import org.graalvm.nativeimage.hosted.RuntimeReflection;
import org.graalvm.nativeimage.hosted.RuntimeResourceAccess;

import java.io.IOException;
import java.lang.reflect.Array;
import java.net.URISyntaxException;
import java.nio.file.Path;
import java.util.Enumeration;
import java.util.List;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

public class XmlBeansFeature implements Feature {

    private static final List<String> SCHEMA_JAR_MARKERS = List.of(
            "org.apache.xmlbeans.XmlObject",
            "org.apache.poi.schemas.ooxml.system.ooxml.TypeSystemHolder"
    );

    @Override
    public void beforeAnalysis(BeforeAnalysisAccess access) {
        for (String markerName : SCHEMA_JAR_MARKERS) {
            final Class<?> marker = access.findClassByName(markerName);
            if (marker == null) {
                throw new IllegalStateException(markerName + " is not on the image class path");
            }
            registerJar(access, marker);
        }
    }

    private static void registerJar(BeforeAnalysisAccess access, Class<?> marker) {
        final Path jar;
        try {
            jar = Path.of(marker.getProtectionDomain().getCodeSource().getLocation().toURI());
        } catch (URISyntaxException e) {
            throw new IllegalStateException("Cannot locate the jar of " + marker.getName(), e);
        }

        int resources = 0;
        int classes = 0;
        int unlinkable = 0;
        try (JarFile jarFile = new JarFile(jar.toFile())) {
            final Enumeration<JarEntry> entries = jarFile.entries();
            while (entries.hasMoreElements()) {
                final String name = entries.nextElement().getName();
                if (name.endsWith(".xsb")) {
                    RuntimeResourceAccess.addResource(marker.getModule(), name);
                    resources++;
                } else if (name.endsWith(".class") && !name.startsWith("META-INF/")
                        && !name.endsWith("module-info.class")) {
                    final String className = name.substring(0, name.length() - ".class".length())
                            .replace('/', '.');
                    final Class<?> type = access.findClassByName(className);
                    if (type == null) {
                        unlinkable++;
                    } else if (register(type)) {
                        classes++;
                    } else {
                        unlinkable++;
                    }
                }
            }
        } catch (IOException e) {
            throw new IllegalStateException("Cannot read " + jar, e);
        }
        System.out.println("XmlBeansFeature: " + jar.getFileName() + ": registered " + resources
                + " schema resources and " + classes + " classes, " + unlinkable
                + " classes cannot be linked against the image class path");
    }

    private static boolean register(Class<?> type) {
        try {
            RuntimeReflection.register(type);
            RuntimeReflection.register(type.getDeclaredConstructors());
            RuntimeReflection.register(type.getDeclaredFields());
            RuntimeReflection.register(Array.newInstance(type, 0).getClass());
            return true;
        } catch (LinkageError e) {
            return false;
        }
    }
}
