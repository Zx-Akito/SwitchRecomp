// Exclusive and ordered loads/stores, with retry loops so host results are deterministic.
stp x29, x30, [sp, #-32]!
movz x1, #0x1111
str x1, [sp, #16]
str xzr, [sp, #24]
add x9, sp, #16
retry:
ldaxr x2, [x9]
add x2, x2, #1
stlxr w3, x2, [x9]
cbnz w3, retry
ldar x4, [x9]
movz w5, #0x77
stlr w5, [x9]
ldar w6, [x9]
ldarb w7, [x9]
retry2:
ldxp x10, x11, [x9]
add x10, x10, #2
stxp w12, x10, x11, [x9]
cbnz w12, retry2
ldr x13, [x9]
movz x9, #0 // x9 held a stack address, which differs between host and guest
ldp x29, x30, [sp], #32
ret
