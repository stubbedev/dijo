package ai.yobix;

import com.oracle.svm.core.annotate.Alias;
import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.tika.ml.chardetect.StructuralEncodingRules;

/**
 * Tika suspects UTF-16 when the bytes at even offsets take three times as many distinct values as
 * those at odd offsets, or the reverse, and once its UTF-16 model agrees, decides on UTF-16
 * without asking its other models. Text in a legacy double-byte encoding, such as Japanese in
 * Shift_JIS, has that shape too. UTF-16 without a byte order mark also holds a NUL byte for every
 * ASCII character (every space, digit and line break), and legacy encodings hold none, so UTF-16
 * is suspected only with one.
 */
@TargetClass(StructuralEncodingRules.class)
final class Target_org_apache_tika_ml_chardetect_StructuralEncodingRules {

    @Alias
    private static int MIN_COLUMN_EVIDENCE_PROBE;

    @Alias
    private static native boolean computeColumnAsymmetry(byte[] bytes);

    @Substitute
    public static boolean has2ByteColumnAsymmetryEvidence(byte[] bytes) {
        if (bytes == null || bytes.length < MIN_COLUMN_EVIDENCE_PROBE) {
            return false;
        }
        for (final byte value : bytes) {
            if (value == 0) {
                return computeColumnAsymmetry(bytes);
            }
        }
        return false;
    }
}
