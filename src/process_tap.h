#ifndef CODEX_MICRO_CHROMA_PROCESS_TAP_H
#define CODEX_MICRO_CHROMA_PROCESS_TAP_H

#include <stddef.h>
#include <stdint.h>

typedef void (*ChromaAudioCallback)(
    const float *left,
    const float *right,
    uint32_t frame_count,
    double sample_rate,
    double sample_time,
    void *context
);

void *chroma_process_tap_start(
    ChromaAudioCallback callback,
    void *context,
    double *sample_rate,
    char *error_buffer,
    size_t error_buffer_size
);

void chroma_process_tap_stop(void *handle);

#endif
