movz x1, #0x1234
movk x1, #0xabcd, lsl #16
movn x2, #0
add x3, x1, x2, lsl #3
and w4, w1, #0xf
orr x5, xzr, #0xff00ff00ff00ff00
eor x6, x1, #0x5555555555555555
sub x7, x2, w1, sxtw
add x8, x1, w2, uxtw #2
lsr w9, w1, #8
asr x10, x2, #4
sbfx x11, x1, #12, #8
ubfiz x12, x1, #4, #12
sxtw x13, w1
cmp w1, #5
cset w14, lt
csel x15, x1, x2, ge
cmp x1, x2
csinc x16, x1, x3, hi
mul x17, x1, x1
smaddl x25, w1, w2, x3
umaddl x19, w1, w2, x3
msub x20, x1, x3, x8
mvn w21, w1
bic x22, x1, x5
tst w1, #0x80000000
cset x23, ne
lsl x24, x1, #60
adds w26, w1, w1
cset w27, hs
cneg x28, x1, mi
