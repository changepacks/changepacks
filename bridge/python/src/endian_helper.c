// Endian conversion helper functions for tree-sitter
// Fix for undefined symbols: le16toh and be16toh
// Works around missing symbols in older Linux toolchains

#include <stdint.h>

// Force our implementation even if system headers define these as macros
// This is necessary for older cross-compilation toolchains
#undef le16toh
#undef be16toh

static uint16_t swap16(uint16_t value) {
    return (uint16_t)((value >> 8) | (value << 8));
}

#if !((defined(__BYTE_ORDER__) && defined(__ORDER_LITTLE_ENDIAN__) && \
       (__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__)) || \
      (defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && \
       (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)) || \
      (defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && \
       (__BYTE_ORDER == __LITTLE_ENDIAN)) || \
      (defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && \
       (__BYTE_ORDER == __BIG_ENDIAN)))
static int host_is_little_endian(void) {
    const uint16_t marker = UINT16_C(1);
    const unsigned char *bytes = (const unsigned char *)&marker;

    return bytes[0] == 1U;
}
#endif

// Provide function symbols that can be linked
// Use visibility attribute to ensure symbols are exported
__attribute__((visibility("default")))
uint16_t le16toh(uint16_t x) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_LITTLE_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__)
    return x;
#elif defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
    return swap16(x);
#elif defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && (__BYTE_ORDER == __LITTLE_ENDIAN)
    return x;
#elif defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && (__BYTE_ORDER == __BIG_ENDIAN)
    return swap16(x);
#else
    return host_is_little_endian() ? x : swap16(x);
#endif
}

__attribute__((visibility("default")))
uint16_t be16toh(uint16_t x) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
    return x;
#elif defined(__BYTE_ORDER__) && defined(__ORDER_LITTLE_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__)
    return swap16(x);
#elif defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && (__BYTE_ORDER == __BIG_ENDIAN)
    return x;
#elif defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && (__BYTE_ORDER == __LITTLE_ENDIAN)
    return swap16(x);
#else
    return host_is_little_endian() ? swap16(x) : x;
#endif
}
