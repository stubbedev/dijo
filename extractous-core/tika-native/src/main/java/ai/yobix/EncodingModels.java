package ai.yobix;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.tika.ml.chardetect.MojibusterEncodingDetector;
import org.apache.tika.ml.chardetect.NaiveBayesBigramEncodingDetector;
import org.apache.tika.ml.junkdetect.JunkDetector;

import java.io.IOException;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;

/**
 * The models of Tika's encoding detectors, read once while the native image is built and kept in
 * its heap. Tika reads them afresh for every encoding detector it makes, one per parser that
 * detects encodings, byte by byte from compressed resources, which in the image costs half a
 * second each time. The models are immutable, so every detector can share one copy.
 */
final class EncodingModels {

    static final JunkDetector JUNK = junkDetector();
    static final NaiveBayesBigramEncodingDetector BIGRAMS = bigrams();

    private EncodingModels() {
    }

    private static JunkDetector junkDetector() {
        try {
            return JunkDetector.loadFromClasspath();
        } catch (IOException e) {
            throw new IllegalStateException("Cannot read Tika's junk detection model", e);
        }
    }

    private static NaiveBayesBigramEncodingDetector bigrams() {
        try {
            final Method load = MojibusterEncodingDetector.class.getDeclaredMethod("loadFromClasspath");
            load.setAccessible(true);
            return (NaiveBayesBigramEncodingDetector) load.invoke(null);
        } catch (NoSuchMethodException | IllegalAccessException | InvocationTargetException e) {
            throw new IllegalStateException("Cannot read Tika's encoding detection model", e);
        }
    }
}

@TargetClass(JunkDetector.class)
final class Target_org_apache_tika_ml_junkdetect_JunkDetector {

    @Substitute
    public static JunkDetector loadFromClasspath() {
        return EncodingModels.JUNK;
    }
}

@TargetClass(MojibusterEncodingDetector.class)
final class Target_org_apache_tika_ml_chardetect_MojibusterEncodingDetector {

    @Substitute
    private static NaiveBayesBigramEncodingDetector loadFromClasspath() {
        return EncodingModels.BIGRAMS;
    }
}
