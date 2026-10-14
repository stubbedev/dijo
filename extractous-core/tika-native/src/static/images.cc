#include <jni.h>

#include <allheaders.h>
#include <libheif/heif.h>

#include <memory>
#include <mutex>
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

std::string heif_failure(const char *attempt, const heif_error &error) {
    return std::string(attempt) + ": " + (error.message != nullptr ? error.message : "unknown error");
}

void heif_init_once() {
    static std::once_flag once;
    std::call_once(once, [] { heif_init(nullptr); });
}

struct HeifContextDeleter {
    void operator()(heif_context *context) const { heif_context_free(context); }
};

struct HeifHandleDeleter {
    void operator()(heif_image_handle *handle) const { heif_image_handle_release(handle); }
};

struct HeifImageDeleter {
    void operator()(heif_image *image) const { heif_image_release(image); }
};

struct PixDeleter {
    void operator()(Pix *pix) const { pixDestroy(&pix); }
};

std::string decode_heif(const std::string &input, const std::string &output) {
    heif_init_once();
    const std::unique_ptr<heif_context, HeifContextDeleter> context(heif_context_alloc());
    if (context == nullptr) {
        return "Cannot allocate a HEIF context";
    }
    heif_error error = heif_context_read_from_file(context.get(), input.c_str(), nullptr);
    if (error.code != heif_error_Ok) {
        return heif_failure("Cannot read the HEIF container", error);
    }
    heif_image_handle *primary = nullptr;
    error = heif_context_get_primary_image_handle(context.get(), &primary);
    if (error.code != heif_error_Ok) {
        return heif_failure("The HEIF file has no primary image", error);
    }
    const std::unique_ptr<heif_image_handle, HeifHandleDeleter> handle(primary);
    const bool alpha = heif_image_handle_has_alpha_channel(handle.get()) != 0;
    heif_image *decoded = nullptr;
    error = heif_decode_image(handle.get(), &decoded, heif_colorspace_RGB,
                              alpha ? heif_chroma_interleaved_RGBA : heif_chroma_interleaved_RGB,
                              nullptr);
    if (error.code != heif_error_Ok) {
        return heif_failure("Cannot decode the primary HEIF image", error);
    }
    const std::unique_ptr<heif_image, HeifImageDeleter> image(decoded);
    const int width = heif_image_get_width(image.get(), heif_channel_interleaved);
    const int height = heif_image_get_height(image.get(), heif_channel_interleaved);
    size_t stride = 0;
    const uint8_t *plane =
        heif_image_get_plane_readonly2(image.get(), heif_channel_interleaved, &stride);
    if (plane == nullptr || width <= 0 || height <= 0) {
        return "The decoded HEIF image has no pixels";
    }
    const std::unique_ptr<Pix, PixDeleter> pix(pixCreate(width, height, 32));
    if (pix == nullptr) {
        return "Cannot allocate a " + std::to_string(width) + "x" + std::to_string(height)
               + " image";
    }
    const int channels = alpha ? 4 : 3;
    pixSetSpp(pix.get(), channels);
    l_uint32 *data = pixGetData(pix.get());
    const l_int32 words = pixGetWpl(pix.get());
    for (int y = 0; y < height; y++) {
        const uint8_t *row = plane + static_cast<size_t>(y) * stride;
        l_uint32 *line = data + static_cast<size_t>(y) * words;
        for (int x = 0; x < width; x++) {
            const uint8_t *sample = row + static_cast<size_t>(x) * channels;
            composeRGBAPixel(sample[0], sample[1], sample[2], alpha ? sample[3] : 255, line + x);
        }
    }
    if (pixWrite(output.c_str(), pix.get(), IFF_PNG) != 0) {
        return "Cannot write the decoded image to " + output;
    }
    return {};
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
