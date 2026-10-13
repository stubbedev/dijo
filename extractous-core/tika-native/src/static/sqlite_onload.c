#include <jni.h>

jint JNI_OnLoad_sqlitejdbc(JavaVM *vm, void *reserved);

int extractous_load_sqlite(void) {
    JavaVM *vm = NULL;
    jsize count = 0;
    if (JNI_GetCreatedJavaVMs(&vm, 1, &count) != JNI_OK || count != 1) {
        return -1;
    }
    return JNI_OnLoad_sqlitejdbc(vm, NULL) > 0 ? 0 : -1;
}
