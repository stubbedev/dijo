#include <jni.h>

#include <allheaders.h>
#include <libdjvu/ddjvuapi.h>
#include <libheif/heif.h>

#include <algorithm>
#include <cstdio>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

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

struct DjvuContextDeleter {
    void operator()(ddjvu_context_t *context) const { ddjvu_context_release(context); }
};

struct DjvuDocumentDeleter {
    void operator()(ddjvu_document_t *document) const { ddjvu_document_release(document); }
};

struct DjvuPageDeleter {
    void operator()(ddjvu_page_t *page) const { ddjvu_page_release(page); }
};

struct DjvuFormatDeleter {
    void operator()(ddjvu_format_t *format) const { ddjvu_format_release(format); }
};

struct FileCloser {
    void operator()(FILE *file) const { std::fclose(file); }
};

void drain(ddjvu_context_t *context, bool wait, std::string &errors) {
    if (wait) {
        ddjvu_message_wait(context);
    }
    while (const ddjvu_message_t *message = ddjvu_message_peek(context)) {
        if (message->m_any.tag == DDJVU_ERROR && message->m_error.message != nullptr) {
            errors += errors.empty() ? "" : "; ";
            errors += message->m_error.message;
        }
        ddjvu_message_pop(context);
    }
}

std::string djvu_failure(const std::string &attempt, const std::string &errors) {
    return errors.empty() ? attempt : attempt + ": " + errors;
}

std::string render_djvu(const std::string &input, int page_number, const std::string &output) {
    const std::unique_ptr<ddjvu_context_t, DjvuContextDeleter> context(
        ddjvu_context_create("extractous"));
    if (context == nullptr) {
        return "Cannot create a DjVu context";
    }
    std::string errors;
    const std::unique_ptr<ddjvu_document_t, DjvuDocumentDeleter> document(
        ddjvu_document_create_by_filename_utf8(context.get(), input.c_str(), 1));
    if (document == nullptr) {
        drain(context.get(), false, errors);
        return djvu_failure("Cannot open the DjVu document", errors);
    }
    while (!ddjvu_document_decoding_done(document.get())) {
        drain(context.get(), true, errors);
    }
    if (ddjvu_document_decoding_error(document.get())) {
        drain(context.get(), false, errors);
        return djvu_failure("Cannot decode the DjVu document", errors);
    }
    const std::string page_name = "page " + std::to_string(page_number);
    const std::unique_ptr<ddjvu_page_t, DjvuPageDeleter> page(
        ddjvu_page_create_by_pageno(document.get(), page_number - 1));
    if (page == nullptr) {
        drain(context.get(), false, errors);
        return djvu_failure("Cannot access " + page_name, errors);
    }
    while (!ddjvu_page_decoding_done(page.get())) {
        drain(context.get(), true, errors);
    }
    if (ddjvu_page_decoding_error(page.get())) {
        drain(context.get(), false, errors);
        return djvu_failure("Cannot decode " + page_name, errors);
    }
    const int width = ddjvu_page_get_width(page.get());
    const int height = ddjvu_page_get_height(page.get());
    if (width <= 0 || height <= 0) {
        return "The DjVu " + page_name + " has no size";
    }
    ddjvu_rect_t rectangle{0, 0, static_cast<unsigned int>(width),
                           static_cast<unsigned int>(height)};
    const std::unique_ptr<ddjvu_format_t, DjvuFormatDeleter> format(
        ddjvu_format_create(DDJVU_FORMAT_GREY8, 0, nullptr));
    if (format == nullptr) {
        return "Cannot create a greyscale DjVu pixel format";
    }
    ddjvu_format_set_row_order(format.get(), 1);
    const auto row = static_cast<size_t>(width);
    std::vector<char> pixels(row * static_cast<size_t>(height), static_cast<char>(0xFF));
    if (!ddjvu_page_render(page.get(), DDJVU_RENDER_COLOR, &rectangle, &rectangle, format.get(),
                           static_cast<unsigned long>(row), pixels.data())) {
        std::fill(pixels.begin(), pixels.end(), static_cast<char>(0xFF));
    }
    const std::unique_ptr<FILE, FileCloser> file(std::fopen(output.c_str(), "wb"));
    if (file == nullptr) {
        return "Cannot open " + output + " for the rendered " + page_name;
    }
    if (std::fprintf(file.get(), "P5\n%d %d\n255\n", width, height) < 0
        || std::fwrite(pixels.data(), 1, pixels.size(), file.get()) != pixels.size()
        || std::fflush(file.get()) != 0) {
        return "Cannot write the rendered " + page_name + " to " + output;
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

JNIEXPORT jbyteArray JNICALL Java_ai_yobix_LinkedImages_djvuPageToPgm(JNIEnv *env, jclass,
                                                                   jbyteArray input, jint page,
                                                                   jbyteArray output) {
    try {
        const std::string error = render_djvu(bytes(env, input), page, bytes(env, output));
        return error.empty() ? nullptr : failure(env, error);
    } catch (const std::exception &e) {
        return failure(env, std::string("Rendering the DjVu page failed: ") + e.what());
    }
}

}
