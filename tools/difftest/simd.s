// SIMD&FP register moves and memory transfers. v8-v15 are callee-saved and unused.
stp x29, x30, [sp, #-64]!
movz x1, #0x1234
movk x1, #0xabcd, lsl #16
movk x1, #0x5555, lsl #32
movi v0.2d, #0
movi v1.16b, #0x5a
movi v2.4s, #0x12, lsl #8
mvni v3.8h, #0x34
movi v4.2s, #0xab, msl #16
fmov v5.4s, #1.5
fmov v6.2d, #-2.0
fmov s7, #0.5
fmov d16, x1
fmov s17, w1
mov v18.16b, v1.16b
mov v18.d[1], x1
mov v19.16b, v2.16b
mov v19.s[2], w1
dup v20.4s, w1
dup v21.8b, w1
fmov x2, d16
fmov w3, s5
umov w4, v1.b[3]
mov x5, v18.d[1]
fmov x6, v18.d[1]
mov v22.8b, v3.8b
fmov s23, s17
fmov d24, d6
stp q1, q18, [sp, #16]
ldp q25, q26, [sp, #16]
str d16, [sp, #48]
ldr s27, [sp, #48]
ldr q28, [sp, #16]
str s17, [sp, #56]
ldur d29, [sp, #52]
ldr x7, [sp, #16]
ldp x29, x30, [sp], #64
ret
