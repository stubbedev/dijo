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

bool is_tiff(int format) {
    return format == IFF_TIFF || format == IFF_TIFF_PACKBITS || format == IFF_TIFF_RLE
           || format == IFF_TIFF_G3 || format == IFF_TIFF_G4 || format == IFF_TIFF_LZW
           || format == IFF_TIFF_JPEG || format == IFF_TIFF_ZIP;
}

bool append_osd(tesseract::TessBaseAPI &api, Pix *pix, int page, std::string &report) {
    api.SetImage(pix);
    const std::unique_ptr<const char[]> osd(api.GetOsdText(page));
    if (osd == nullptr) {
        return false;
    }
    report += osd.get();
    return true;
}

bool detect_pages(tesseract::TessBaseAPI &api, const std::string &image, std::string &report) {
    int format = IFF_UNKNOWN;
    if (findFileFormat(image.c_str(), &format) != 0 || format == IFF_UNKNOWN) {
        return false;
    }
    api.SetInputName(image.c_str());
    if (!is_tiff(format)) {
        Pix *pix = pixRead(image.c_str());
        if (pix == nullptr) {
            return false;
        }
        const bool detected = append_osd(api, pix, 0, report);
        pixDestroy(&pix);
        return detected;
    }
    size_t offset = 0;
    for (int page = 0;; page++) {
        Pix *pix = pixReadFromMultipageTiff(image.c_str(), &offset);
        if (pix == nullptr) {
            return true;
        }
        const bool detected = append_osd(api, pix, page, report);
        pixDestroy(&pix);
        if (!detected) {
            return false;
        }
        if (offset == 0) {
            return true;
        }
    }
}

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
        api.SetOutputName(output.c_str());
        api.SetPageSegMode(static_cast<tesseract::PageSegMode>(pageSegMode));
        tesseract::TessTextRenderer renderer(output.c_str());
        if (!renderer.happy()) {
            return FAILED;
        }
        const auto start = std::chrono::steady_clock::now();
        const bool succeeded = api.ProcessPages(input.c_str(), nullptr, timeoutMillis, &renderer);
        const auto elapsed = std::chrono::steady_clock::now() - start;
        if (succeeded) {
            return SUCCEEDED;
        }
        return elapsed >= std::chrono::milliseconds(timeoutMillis) ? TIMED_OUT : FAILED;
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_meanConfidence(JNIEnv *, jclass,
                                                                    jlong engine) {
    try {
        return reinterpret_cast<tesseract::TessBaseAPI *>(engine)->MeanTextConf();
    } catch (...) {
        return 0;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detect(JNIEnv *env, jclass,
                                                                  jlong engine,
                                                                  jbyteArray image) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        api.SetPageSegMode(tesseract::PSM_OSD_ONLY);
        std::string report;
        if (!detect_pages(api, bytes(env, image), report)) {
            return nullptr;
        }
        const auto length = static_cast<jsize>(report.size());
        jbyteArray result = env->NewByteArray(length);
        if (result != nullptr) {
            env->SetByteArrayRegion(result, 0, length,
                                    reinterpret_cast<const jbyte *>(report.data()));
        }
        return result;
    } catch (...) {
        return nullptr;
    }
}

}
