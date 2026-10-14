#include <jni.h>

#include <allheaders.h>
#include <tesseract/baseapi.h>
#include <tesseract/ocrclass.h>
#include <omp.h>

#include "pageres.h"

#include <algorithm>
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
constexpr jint GAVE_UP = 3;

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

/** The words an engine has read so far on a page and the sum of their confidences. */
struct WordsRead {
    int words;
    long confidences;
};

/** An engine that can tell, while it reads a page, how well it read the words so far. */
class Engine : public tesseract::TessBaseAPI {
public:
    /**
     * The words recognized so far, each with its confidence as MeanTextConf takes it: 100 plus
     * five times its certainty, within 0 to 100.
     */
    WordsRead read_so_far() const {
        WordsRead read{0, 0};
        tesseract::PAGE_RES_IT word(const_cast<tesseract::PAGE_RES *>(GetPageRes()));
        for (word.restart_page(); word.word() != nullptr; word.forward()) {
            const tesseract::WERD_RES *result = word.word();
            if (result->tess_failed || result->best_choice == nullptr) {
                continue;
            }
            const int confidence = static_cast<int>(100 + 5 * result->best_choice->certainty());
            read.words++;
            read.confidences += std::clamp(confidence, 0, 100);
        }
        return read;
    }
};

Engine &engine_of(jlong engine) {
    return *reinterpret_cast<Engine *>(engine);
}

/**
 * When to give up reading a page: once afterWords words are read, if their mean confidence is
 * below belowConfidence. It is judged once, so a page read well at first is read to its end.
 */
struct GiveUp {
    const Engine &engine;
    int afterWords;
    int belowConfidence;
    bool judged;
    bool givenUp;
};

bool give_up_when_poor(void *context, int) {
    auto &giveUp = *static_cast<GiveUp *>(context);
    if (giveUp.judged) {
        return false;
    }
    const WordsRead read = giveUp.engine.read_so_far();
    if (read.words < giveUp.afterWords) {
        return false;
    }
    giveUp.judged = true;
    giveUp.givenUp = read.confidences < static_cast<long>(giveUp.belowConfidence) * read.words;
    return giveUp.givenUp;
}

bool is_tiff(int format) {
    return format == IFF_TIFF || format == IFF_TIFF_PACKBITS || format == IFF_TIFF_RLE
           || format == IFF_TIFF_G3 || format == IFF_TIFF_G4 || format == IFF_TIFF_LZW
           || format == IFF_TIFF_JPEG || format == IFF_TIFF_ZIP;
}

/**
 * Hands each page of an image file to visit(pix, page), as ProcessPages reads them: every page
 * of a TIFF, the one image of another format. Stops at the first page visit refuses; false when
 * the file cannot be read.
 */
template <typename Visit> bool each_page(const std::string &image, Visit visit) {
    int format = IFF_UNKNOWN;
    if (findFileFormat(image.c_str(), &format) != 0 || format == IFF_UNKNOWN) {
        return false;
    }
    if (!is_tiff(format)) {
        const PixPointer pix(pixRead(image.c_str()));
        return pix != nullptr && visit(pix.get(), 0);
    }
    size_t offset = 0;
    for (int page = 0;; page++) {
        const PixPointer pix(pixReadFromMultipageTiff(image.c_str(), &offset));
        if (pix == nullptr) {
            return page > 0;
        }
        if (!visit(pix.get(), page)) {
            return false;
        }
        if (offset == 0) {
            return true;
        }
    }
}

/**
 * Recognizes a page as ProcessPage does and appends its text as the text renderer does: after
 * the page separator unless it is the first page. With giveUpAfterWords above 0, gives up as
 * GiveUp tells. The recognizer divides its work among threads.
 */
jint recognize(Engine &api, Pix *pix, int page, jint giveUpAfterWords,
               jint giveUpBelowConfidence, jint threads, jint timeoutMillis, std::string &text) {
    omp_set_num_threads(threads);
    api.SetImage(pix);
    GiveUp giveUp{api, giveUpAfterWords, giveUpBelowConfidence, false, false};
    tesseract::ETEXT_DESC monitor;
    if (giveUpAfterWords > 0) {
        monitor.cancel = give_up_when_poor;
        monitor.cancel_this = &giveUp;
    }
    if (timeoutMillis > 0) {
        monitor.set_deadline_msecs(timeoutMillis);
    }
    const auto start = std::chrono::steady_clock::now();
    if (api.Recognize(&monitor) < 0) {
        if (giveUp.givenUp) {
            return GAVE_UP;
        }
        const auto elapsed = std::chrono::steady_clock::now() - start;
        return timeoutMillis > 0 && elapsed >= std::chrono::milliseconds(timeoutMillis)
                   ? TIMED_OUT
                   : FAILED;
    }
    const std::unique_ptr<char[]> utf8(api.GetUTF8Text());
    if (utf8 == nullptr) {
        return FAILED;
    }
    const char *separator = api.GetStringVariable("page_separator");
    if (page > 0 && separator != nullptr) {
        text += separator;
    }
    text += utf8.get();
    return SUCCEEDED;
}

jint hand_over(JNIEnv *env, jint status, const std::string &text, jobjectArray result) {
    if (status != SUCCEEDED) {
        return status;
    }
    jbyteArray bytes = utf8_bytes(env, text.data(), text.size());
    if (bytes == nullptr) {
        return FAILED;
    }
    env->SetObjectArrayElement(result, 0, bytes);
    return SUCCEEDED;
}

/** Reads pixels as one page and puts its text into result[0]. */
jint read_pix(JNIEnv *env, Engine &api, Pix *pix, jint pageSegMode, jobjectArray names,
              jobjectArray values, jint giveUpAfterWords, jint giveUpBelowConfidence,
              jint threads, jint timeoutMillis, jobjectArray result) {
    if (pix == nullptr) {
        return FAILED;
    }
    const Variables variables(api, bytes(env, names), bytes(env, values));
    api.SetPageSegMode(static_cast<tesseract::PageSegMode>(pageSegMode));
    std::string text;
    return hand_over(env,
                     recognize(api, pix, 0, giveUpAfterWords, giveUpBelowConfidence, threads,
                               timeoutMillis, text),
                     text, result);
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

jbyteArray detect_pix(JNIEnv *env, tesseract::TessBaseAPI &api, Pix *pix, jobjectArray names,
                      jobjectArray values) {
    if (pix == nullptr) {
        return nullptr;
    }
    const Variables variables(api, bytes(env, names), bytes(env, values));
    api.SetPageSegMode(tesseract::PSM_OSD_ONLY);
    std::string report;
    if (!append_osd(api, pix, 0, report)) {
        return nullptr;
    }
    return utf8_bytes(env, report.data(), report.size());
}

}

extern "C" {

JNIEXPORT jlong JNICALL Java_ai_yobix_LinkedTesseract_create(JNIEnv *env, jclass,
                                                             jbyteArray tessdata,
                                                             jbyteArray language) {
    try {
        quiet_once();
        auto api = std::make_unique<Engine>();
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

JNIEXPORT void JNICALL Java_ai_yobix_LinkedTesseract_destroy(JNIEnv *, jclass, jlong engine) {
    delete &engine_of(engine);
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_runFile(
    JNIEnv *env, jclass, jlong engine, jbyteArray image, jint pageSegMode, jobjectArray names,
    jobjectArray values, jint giveUpAfterWords, jint giveUpBelowConfidence, jint threads,
    jint timeoutMillis, jobjectArray result) {
    try {
        auto &api = engine_of(engine);
        const std::string input = bytes(env, image);
        const Variables variables(api, bytes(env, names), bytes(env, values));
        api.SetPageSegMode(static_cast<tesseract::PageSegMode>(pageSegMode));
        api.SetInputName(input.c_str());
        std::string text;
        jint status = FAILED;
        if (!each_page(input, [&](Pix *pix, int page) {
                status = recognize(api, pix, page, giveUpAfterWords, giveUpBelowConfidence,
                                   threads, timeoutMillis, text);
                return status == SUCCEEDED;
            })) {
            return status == SUCCEEDED ? FAILED : status;
        }
        return hand_over(env, status, text, result);
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_runGray(
    JNIEnv *env, jclass, jlong engine, jbyteArray gray, jint width, jint height,
    jint pageSegMode, jobjectArray names, jobjectArray values, jint giveUpAfterWords,
    jint giveUpBelowConfidence, jint threads, jint timeoutMillis, jobjectArray text) {
    try {
        auto &api = engine_of(engine);
        const PixPointer pix = gray_pix(env, gray, width, height);
        return read_pix(env, api, pix.get(), pageSegMode, names, values, giveUpAfterWords,
                        giveUpBelowConfidence, threads, timeoutMillis, text);
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_runRgb(
    JNIEnv *env, jclass, jlong engine, jintArray rgb, jint width, jint height,
    jint pageSegMode, jobjectArray names, jobjectArray values, jint giveUpAfterWords,
    jint giveUpBelowConfidence, jint threads, jint timeoutMillis, jobjectArray text) {
    try {
        auto &api = engine_of(engine);
        const PixPointer pix = rgb_pix(env, rgb, width, height);
        return read_pix(env, api, pix.get(), pageSegMode, names, values, giveUpAfterWords,
                        giveUpBelowConfidence, threads, timeoutMillis, text);
    } catch (...) {
        return FAILED;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detectGray(JNIEnv *env, jclass,
                                                                      jlong engine,
                                                                      jbyteArray gray,
                                                                      jint width, jint height,
                                                                      jobjectArray names,
                                                                      jobjectArray values) {
    try {
        auto &api = engine_of(engine);
        const PixPointer pix = gray_pix(env, gray, width, height);
        return detect_pix(env, api, pix.get(), names, values);
    } catch (...) {
        return nullptr;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detectRgb(JNIEnv *env, jclass,
                                                                     jlong engine,
                                                                     jintArray rgb, jint width,
                                                                     jint height,
                                                                     jobjectArray names,
                                                                     jobjectArray values) {
    try {
        auto &api = engine_of(engine);
        const PixPointer pix = rgb_pix(env, rgb, width, height);
        return detect_pix(env, api, pix.get(), names, values);
    } catch (...) {
        return nullptr;
    }
}

JNIEXPORT jint JNICALL Java_ai_yobix_LinkedTesseract_meanConfidence(JNIEnv *, jclass,
                                                                    jlong engine) {
    try {
        return engine_of(engine).MeanTextConf();
    } catch (...) {
        return 0;
    }
}

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedTesseract_detect(JNIEnv *env, jclass,
                                                                  jlong engine,
                                                                  jbyteArray image,
                                                                  jobjectArray names,
                                                                  jobjectArray values) {
    try {
        auto &api = engine_of(engine);
        const Variables variables(api, bytes(env, names), bytes(env, values));
        api.SetPageSegMode(tesseract::PSM_OSD_ONLY);
        const std::string input = bytes(env, image);
        api.SetInputName(input.c_str());
        std::string report;
        if (!each_page(input, [&](Pix *pix, int page) {
                return append_osd(api, pix, page, report);
            })) {
            return nullptr;
        }
        return utf8_bytes(env, report.data(), report.size());
    } catch (...) {
        return nullptr;
    }
}

}
