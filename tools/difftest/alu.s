// Division (incl. by zero and overflow), high multiply, register shifts, 1-source ops, BFM.
movz x1, #0x1234
movk x1, #0xfedc, lsl #16
movk x1, #0x8765, lsl #32
movk x1, #0xc001, lsl #48
movz x2, #7
movn x3, #0
movz x4, #0x8000, lsl #48
udiv x5, x1, x2
sdiv x6, x1, x2
sdiv x7, x4, x3
udiv x8, x1, xzr
sdiv w9, w1, w2
udiv w10, w1, w3
umulh x11, x1, x1
smulh x12, x1, x3
movz x13, #67
lsl x14, x1, x13
lsr x15, x1, x13
asr x17, x1, x13
ror x19, x1, x13
lsl w20, w1, w13
asr w21, w1, w2
ror w22, w1, w2
clz x23, x2
cls x24, x1
rbit w25, w1
rev x26, x1
rev16 w27, w1
rev32 x28, x1
mov x0, x1
bfi x0, x2, #13, #9
mov x2, x1
bfxil w2, w3, #3, #10
mov x3, x1
bfi w3, w1, #28, #4
clz w4, w4
ret
