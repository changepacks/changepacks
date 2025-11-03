// Endian conversion helper functions for tree-sitter
// Fix for undefined symbols: le16toh and be16toh
// Works around missing symbols in older Linux toolchains

#include <stdint.h>

// Force our implementation even if system headers define these as macros
// This is necessary for older cross-compilation toolchains
#undef le16toh
#undef be16toh

// Provide function symbols that can be linked
// Use visibility attribute to ensure symbols are exported
__attribute__((visibility("default"))) 
uint16_t le16toh(uint16_t x) {
    // Simple byte swap for little-endian to host conversion
    // On little-endian systems (most ARM/x86), this is a no-op
    // On big-endian systems, swap bytes
#if defined(__BYTE_ORDER__) && defined(__ORDER_LITTLE_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__)
    return x;
#elif defined(__BYTE_ORDER) && defined(__LITTLE_ENDIAN) && (__BYTE_ORDER == __LITTLE_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}

__attribute__((visibility("default"))) 
uint16_t be16toh(uint16_t x) {
    // Simple byte swap for big-endian to host conversion
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
    return x;
#elif defined(__BYTE_ORDER) && defined(__BIG_ENDIAN) && (__BYTE_ORDER == __BIG_ENDIAN)
    return x;
#else
    return (uint16_t)(((x & 0xff00) >> 8) | ((x & 0x00ff) << 8));
#endif
}


