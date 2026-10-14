#if defined(__x86_64__)

#include <emmintrin.h>
#include <stddef.h>
#include <stdint.h>

typedef uint64_t __attribute__((__may_alias__, __aligned__(1))) unaligned64;
typedef uint32_t __attribute__((__may_alias__, __aligned__(1))) unaligned32;
typedef uint16_t __attribute__((__may_alias__, __aligned__(1))) unaligned16;

enum { STRING_COPY_FROM = 4096 };

static inline __m128i load(const unsigned char *at) {
    return _mm_loadu_si128((const __m128i *)at);
}

static inline void store(unsigned char *at, __m128i value) {
    _mm_storeu_si128((__m128i *)at, value);
}

__attribute__((optimize("no-tree-loop-distribute-patterns")))
void *memcpy(void *restrict destination, const void *restrict source, size_t size) {
    unsigned char *to = destination;
    const unsigned char *from = source;
    if (size <= 16) {
        if (size >= 8) {
            const uint64_t head = *(const unaligned64 *)from;
            const uint64_t tail = *(const unaligned64 *)(from + size - 8);
            *(unaligned64 *)to = head;
            *(unaligned64 *)(to + size - 8) = tail;
        } else if (size >= 4) {
            const uint32_t head = *(const unaligned32 *)from;
            const uint32_t tail = *(const unaligned32 *)(from + size - 4);
            *(unaligned32 *)to = head;
            *(unaligned32 *)(to + size - 4) = tail;
        } else if (size >= 2) {
            const uint16_t head = *(const unaligned16 *)from;
            const uint16_t tail = *(const unaligned16 *)(from + size - 2);
            *(unaligned16 *)to = head;
            *(unaligned16 *)(to + size - 2) = tail;
        } else if (size == 1) {
            *to = *from;
        }
        return destination;
    }
    if (size <= 32) {
        const __m128i head = load(from);
        const __m128i tail = load(from + size - 16);
        store(to, head);
        store(to + size - 16, tail);
        return destination;
    }
    if (size >= STRING_COPY_FROM) {
        __asm__ volatile("rep movsb" : "+D"(to), "+S"(from), "+c"(size) : : "memory");
        return destination;
    }
    const __m128i tail0 = load(from + size - 32);
    const __m128i tail1 = load(from + size - 16);
    for (size_t offset = 0; offset + 32 <= size; offset += 32) {
        const __m128i first = load(from + offset);
        const __m128i second = load(from + offset + 16);
        store(to + offset, first);
        store(to + offset + 16, second);
    }
    store(to + size - 32, tail0);
    store(to + size - 16, tail1);
    return destination;
}

__attribute__((visibility("hidden"), alias("memcpy")))
void *__memcpy_fwd(void *destination, const void *source, size_t size);

#endif
