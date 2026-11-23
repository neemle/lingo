#ifndef READER_H
#define READER_H
#include <stdint.h>
#include <stddef.h>
/* v1 synchronous JSON UTF-8. Borrow input for call only. Read returned bytes before
 * releasing allocation once. Never access bytes concurrently with release.
 * All handles/allocation IDs are opaque, non-reused, process-local integers.
 * A non-null input pointer MUST designate len readable bytes; arbitrary addresses
 * cannot be validated. null is permitted only with zero length.
 * Rust serializes Store calls and close. No caller may retain a Store pointer.
 * Missing result allocation (all zero) means catastrophic bridge failure.
 */
typedef struct { const uint8_t *data; uint64_t len; uint64_t allocation; } ReaderBuffer;
uint32_t reader_abi_version(void);
ReaderBuffer reader_call(uint64_t handle, const uint8_t *input, uint64_t len);
/* Width of a UTF-16 range, or line height when length is zero. Invoked synchronously. */
typedef float (*ReaderMeasureFont)(void *context, uint32_t start, uint32_t length, float size);
ReaderBuffer reader_fit_pdf_text(const uint8_t *input, uint64_t len, float width, float height,
    float preferred, float minimum, void *context, ReaderMeasureFont measure);
/* Returns raw PNG bytes, or an empty buffer on failure. */
ReaderBuffer reader_rotate_ocr_png(const uint8_t *input, uint64_t len, uint16_t clockwise_angle);
uint32_t reader_close(uint64_t handle);
uint32_t reader_release(uint64_t allocation);
#endif
