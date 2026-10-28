// Prints the registers listed in REGS / VREGS after running a sequence natively (run.sh).
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

void run(uint64_t *out);

int main(void) {
    uint64_t out[128];
    run(out);
    const char *p = REGS;
    char *end;
    int k = 0;
    for (long r = strtol(p, &end, 10); end != p; p = end, r = strtol(p, &end, 10)) {
        printf("x%ld=%016llx\n", r, (unsigned long long)out[k++]);
    }
    p = VREGS;
    for (long r = strtol(p, &end, 10); end != p; p = end, r = strtol(p, &end, 10)) {
        printf("v%ld=%016llx%016llx\n", r, (unsigned long long)out[k + 1],
               (unsigned long long)out[k]);
        k += 2;
    }
    return 0;
}
