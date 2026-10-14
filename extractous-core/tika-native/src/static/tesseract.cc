#include <jni.h>

#include <allheaders.h>
#include <tesseract/baseapi.h>
#include <tesseract/ocrclass.h>
#include <tesseract/renderer.h>

#include <chrono>
#include <cstring>
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

struct PixDeleter {
    void operator()(Pix *pix) const { pixDestroy(&pix); }
};

using PixPointer = std::unique_ptr<Pix, PixDeleter>;

PixPointer gray_pix(JNIEnv *env, jbyteArray levels, jint width, jint height) {
    PixPointer pix(pixCreate(width, height, 8));
    if (pix == nullptr) {
        return pix;
    }
    jbyte *bytes = env->GetByteArrayElements(levels, nullptr);
    l_uint32 *data = pixGetData(pix.get());
    const l_int32 words = pixGetWpl(pix.get());
    for (jint y = 0; y < height; y++) {
        l_uint32 *line = data + static_cast<size_t>(y) * words;
        const jbyte *row = bytes + static_cast<size_t>(y) * width;
        for (jint x = 0; x < width; x++) {
            SET_DATA_BYTE(line, x, static_cast<l_uint8>(row[x]));
        }
    }
    env->ReleaseByteArrayElements(levels, bytes, JNI_ABORT);
    return pix;
}

PixPointer rgb_pix(JNIEnv *env, jintArray rgb, jint width, jint height) {
    PixPointer pix(pixCreate(width, height, 32));
    if (pix == nullptr) {
        return pix;
    }
    jint *packed = env->GetIntArrayElements(rgb, nullptr);
    l_uint32 *data = pixGetData(pix.get());
    const l_int32 words = pixGetWpl(pix.get());
    for (jint y = 0; y < height; y++) {
        l_uint32 *line = data + static_cast<size_t>(y) * words;
        const jint *row = packed + static_cast<size_t>(y) * width;
        for (jint x = 0; x < width; x++) {
            const auto value = static_cast<l_uint32>(row[x]);
            composeRGBPixel((value >> 16) & 0xFF, (value >> 8) & 0xFF, value & 0xFF, line + x);
        }
    }
    env->ReleaseIntArrayElements(rgb, packed, JNI_ABORT);
    return pix;
}

jbyteArray utf8_bytes(JNIEnv *env, const char *text, size_t length) {
    const auto size = static_cast<jsize>(length);
    jbyteArray result = env->NewByteArray(size);
    if (result != nullptr) {
        env->SetByteArrayRegion(result, 0, size, reinterpret_cast<const jbyte *>(text));
    }
    return result;
}

/** Reads a page as ProcessPage does, without a renderer: its text is the text renderer's. */
jint read_pix(JNIEnv *env, tesseract::TessBaseAPI &api, Pix *pix, jint pageSegMode,
              jobjectArray names, jobjectArray values, jint timeoutMillis, jobjectArray text) {
    if (pix == nullptr) {
        return FAILED;
    }
    const Variables variables(api, bytes(env, names), bytes(env, values));
    api.SetPageSegMode(static_cast<tesseract::PageSegMode>(pageSegMode));
    api.SetImage(pix);
    const auto start = std::chrono::steady_clock::now();
    bool failed;
    if (timeoutMillis > 0) {
        tesseract::ETEXT_DESC monitor;
        monitor.cancel = nullptr;
        monitor.cancel_this = nullptr;
        monitor.set_deadline_msecs(timeoutMillis);
        failed = api.Recognize(&monitor) < 0;
    } else {
        failed = api.Recognize(nullptr) < 0;
    }
    if (failed) {
        const auto elapsed = std::chrono::steady_clock::now() - start;
        return timeoutMillis > 0 && elapsed >= std::chrono::milliseconds(timeoutMillis)
                   ? TIMED_OUT
                   : FAILED;
    }
    const std::unique_ptr<char[]> utf8(api.GetUTF8Text());
    if (utf8 == nullptr) {
        return FAILED;
    }
    jbyteArray result = utf8_bytes(env, utf8.get(), std::strlen(utf8.get()));
    if (result == nullptr) {
        return FAILED;
    }
    env->SetObjectArrayElement(text, 0, result);
    return SUCCEEDED;
}

bool append_osd(tesseract::TessBaseAPI &api, Pix *pix, int page, std::string &report);

jbyteArray detect_pix(JNIEnv *env, tesseract::TessBaseAPI &api, Pix *pix) {
    if (pix == nullptr) {
        return nullptr;
    }
    api.SetPageSegMode(tesseract::PSM_OSD_ONLY);
    std::string report;
    if (!append_osd(api, pix, 0, report)) {
        return nullptr;
    }
    return utf8_bytes(env, report.data(), report.size());
}

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

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_runGray(
    JNIEnv *env, jclass, jlong engine, jbyteArray gray, jint width, jint height,
    jint pageSegMode, jobjectArray names, jobjectArray values, jint timeoutMillis,
    jobjectArray text) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        const PixPointer pix = gray_pix(env, gray, width, height);
        return read_pix(env, api, pix.get(), pageSegMode, names, values, timeoutMillis, text);
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_runRgb(
    JNIEnv *env, jclass, jlong engine, jintArray rgb, jint width, jint height,
    jint pageSegMode, jobjectArray names, jobjectArray values, jint timeoutMillis,
    jobjectArray text) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        const PixPointer pix = rgb_pix(env, rgb, width, height);
        return read_pix(env, api, pix.get(), pageSegMode, names, values, timeoutMillis, text);
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detectGray(JNIEnv *env, jclass,
                                                                      jlong engine,
                                                                      jbyteArray gray,
                                                                      jint width, jint height) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        const PixPointer pix = gray_pix(env, gray, width, height);
        return detect_pix(env, api, pix.get());
    } catch (...) {
        return nullptr;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detectRgb(JNIEnv *env, jclass,
                                                                     jlong engine,
                                                                     jintArray rgb, jint width,
                                                                     jint height) {
    try {
        auto &api = *reinterpret_cast<tesseract::TessBaseAPI *>(engine);
        const PixPointer pix = rgb_pix(env, rgb, width, height);
        return detect_pix(env, api, pix.get());
    } catch (...) {
        return nullptr;
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
