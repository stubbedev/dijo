package ai.yobix;

import com.oracle.svm.core.annotate.Alias;
import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;

/**
 * sqlite-jdbc's JNI library is linked into the static executable, which cannot load libraries,
 * and GraalVM only finds the load hook of a statically linked JDK library. So instead of loading
 * the library, the loader runs its load hook directly, once.
 */
@TargetClass(className = "org.sqlite.SQLiteJDBCLoader")
final class Target_org_sqlite_SQLiteJDBCLoader {

    @Alias
    private static boolean extracted;

    @Substitute
    private static void loadSQLiteNativeLibrary() throws Exception {
        if (extracted) {
            return;
        }
        if (StaticSqlite.load() != 0) {
            throw new Exception("The statically linked sqlite-jdbc library did not initialize");
        }
        extracted = true;
    }
}
