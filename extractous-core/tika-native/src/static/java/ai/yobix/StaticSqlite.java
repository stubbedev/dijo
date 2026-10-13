package ai.yobix;

import org.graalvm.nativeimage.c.function.CFunction;

/** The load hook of sqlite-jdbc's JNI library, linked into the static executable. */
final class StaticSqlite {

    private StaticSqlite() {
    }

    /** Runs the library's JNI_OnLoad: 0 when it succeeded. */
    @CFunction("extractous_load_sqlite")
    static native int load();
}
