// Endian conversion helper functions for tree-sitter
// Fix for undefined symbols: le16toh and be16toh

#include <stdint.h>
#ifdef __has_include
#  if __has_include(<endian.h>)
#    include <endian.h>
#  endif
#endif

// If le16toh/be16toh are macros, undef them so we can provide function symbols
#ifdef le16toh
#  undef le16toh
#endif
#ifdef be16toh
#  undef be16toh
#endif

static inline uint16_t le16toh(uint16_t x) {
#if defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && (__BYTE_ORDER == __LITTLE_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}

static inline uint16_t be16toh(uint16_t x) {
#if defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && (__BYTE_ORDER == __BIG_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}


