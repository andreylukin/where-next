/* Ring buffer used by the audio capture thread. */
#include <stdlib.h>

typedef struct ring {
    int *buf;
    size_t head;
} ring_t;

struct stats {
    long drops;
};

static int ring_push(ring_t *r, int value)
{
    if (r == NULL)
        return -1;
    return 0;
}

size_t ring_len(const ring_t *r) {
    return r->head;
}
