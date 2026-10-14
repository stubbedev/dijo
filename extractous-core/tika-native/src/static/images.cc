#include <jni.h>

#include "heif.h"

#include <string>

namespace {

std::string bytes(JNIEnv *env, jbyteArray array) {
    const jsize length = env->GetArrayLength(array);
    std::string text(static_cast<size_t>(length), '\0');
    env->GetByteArrayRegion(array, 0, length, reinterpret_cast<jbyte *>(text.data()));
    return text;
}

jbyteArray failure(JNIEnv *env, const std::string &message) {
    const auto length = static_cast<jsize>(message.size());
    jbyteArray result = env->NewByteArray(length);
    if (result != nullptr) {
        env->SetByteArrayRegion(result, 0, length,
                                reinterpret_cast<const jbyte *>(message.data()));
    }
    return result;
}

}

extern "C" {

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedImages_heifToPng(JNIEnv *env, jclass,
                                                                   jbyteArray input,
                                                                   jbyteArray output) {
    try {
        const std::string error = decode_heif(bytes(env, input), bytes(env, output));
        return error.empty() ? nullptr : failure(env, error);
    } catch (const std::exception &e) {
        return failure(env, std::string("Decoding the HEIF image failed: ") + e.what());
    }
}

}
