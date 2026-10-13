#include <jni.h>

#include <allheaders.h>
#include <tesseract/baseapi.h>
#include <tesseract/renderer.h>

#include <chrono>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

namespace {

constexpr jint SUCCEEDED = 0;
constexpr jint FAILED = 1;
constexpr jint TIMED_OUT = 2;

std::string bytes(JNIEnv *env, jbyteArray array) {
    const jsize length = env->GetArrayLength(array);
    std::string text(static_cast<size_t>(length), '\0');
    env->GetByteArrayRegion(array, 0, length, reinterpret_cast<jbyte *>(text.data()));
    return text;
}

std::vector<std::string> bytes(JNIEnv *env, jobjectArray arrays) {
    const jsize count = env->GetArrayLength(arrays);
    std::vector<std::string> texts;
    texts.reserve(static_cast<size_t>(count));
    for (jsize index = 0; index < count; index++) {
        auto *array = static_cast<jbyteArray>(env->GetObjectArrayElement(arrays, index));
        texts.push_back(bytes(env, array));
        env->DeleteLocalRef(array);
    }
    return texts;
}

void quiet_once() {
    static std::once_flag once;
    std::call_once(once, [] { setMsgSeverity(L_SEVERITY_NONE); });
}

class Variables {
public:
    Variables(tesseract::TessBaseAPI &api, const std::vector<std::string> &names,
              const std::vector<std::string> &values)
        : api_(api) {
        for (size_t index = 0; index < names.size(); index++) {
            std::string previous;
            if (api_.GetVariableAsString(names[index].c_str(), &previous)
                && api_.SetVariable(names[index].c_str(), values[index].c_str())) {
                previous_.emplace_back(names[index], previous);
            }
        }
    }

    ~Variables() {
        for (const auto &[name, value] : previous_) {
            api_.SetVariable(name.c_str(), value.c_str());
        }
    }

private:
    tesseract::TessBaseAPI &api_;
    std::vector<std::pair<std::string, std::string>> previous_;
};

}

extern "C" {

JNIEXPORT jlong JNICALL Java_ai_yobix_LinkedTesseract_create(JNIEnv *env, jclass,
                                                             jbyteArray tessdata,
                                                             jbyteArray language) {
    try {
        quiet_once();
        auto api = std::make_unique<tesseract::TessBaseAPI>();
        api->SetVariable("debug_file", "/dev/null");
        if (api->Init(bytes(env, tessdata).c_str(), bytes(env, language).c_str(),
                      tesseract::OEM_DEFAULT) != 0) {
            return 0;
        }
        return reinterpret_cast<jlong>(api.release());
    } catch (...) {
        return 0;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_run(JNIEnv *env, jclass, jlong engine,
                                                         jbyteArray image, jbyteArray outputBase,
                                                         jint pageSegMode, jobjectArray names,
                                                         jobjectArray values, jint timeoutMillis) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        const std::string input = bytes(env, image);
        const std::string output = bytes(env, outputBase);
        const Variables variables(api, bytes(env, names), bytes(env, values));
        const auto mode = static_cast<tesseract::PageSegMode>(pageSegMode);
        api.SetOutputName(output.c_str());
        api.SetPageSegMode(mode);
        std::unique_ptr<tesseract::TessResultRenderer> renderer;
        if (mode == tesseract::PSM_OSD_ONLY) {
            renderer = std::make_unique<tesseract::TessOsdRenderer>(output.c_str());
        } else {
            renderer = std::make_unique<tesseract::TessTextRenderer>(output.c_str());
        }
        if (!renderer->happy()) {
            return FAILED;
        }
        const auto start = std::chrono::steady_clock::now();
        const bool succeeded = api.ProcessPages(input.c_str(), nullptr, timeoutMillis,
                                                renderer.get());
        const auto elapsed = std::chrono::steady_clock::now() - start;
        if (succeeded) {
            return SUCCEEDED;
        }
        return elapsed >= std::chrono::milliseconds(timeoutMillis) ? TIMED_OUT : FAILED;
    } catch (...) {
        return FAILED;
    }
}

}
