// Prints the registers listed in REGS after running seq.s natively (see run.sh).
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

void run(uint64_t *out);

int main(void) {
    uint64_t out[32];
    run(out);
    const char *p = REGS;
    char *end;
    int k = 0;
    for (long r = strtol(p, &end, 10); end != p; p = end, r = strtol(p, &end, 10)) {
        printf("x%ld=%016llx\n", r, (unsigned long long)out[k++]);
    }
    return 0;
}
