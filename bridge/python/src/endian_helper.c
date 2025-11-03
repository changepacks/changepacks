// Endian conversion helper functions for tree-sitter
// Fix for undefined symbols: le16toh and be16toh

#include <stdint.h>
#ifdef __has_include
#  if __has_include(<endian.h>)
#    include <endian.h>
#  endif
#endif

// inline 구현으로 충돌을 피합니다.
static inline uint16_t le16toh_impl(uint16_t x) {
#if defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && (__BYTE_ORDER == __LITTLE_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}

static inline uint16_t be16toh_impl(uint16_t x) {
#if defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && (__BYTE_ORDER == __BIG_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}

// 링커가 찾을 수 있는 심볼 제공(이미 있을 경우를 대비해 weak 지정)
__attribute__((weak)) uint16_t le16toh(uint16_t x) {
    return le16toh_impl(x);
}

__attribute__((weak)) uint16_t be16toh(uint16_t x) {
    return be16toh_impl(x);
}


