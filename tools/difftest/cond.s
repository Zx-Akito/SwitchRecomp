// Conditional compares: both condition outcomes, register/immediate, 32/64-bit.
movz x1, #5
movz x2, #7
cmp x1, x2
ccmp x1, #5, #4, ne
cset x3, eq
cmp x1, x1
ccmp x1, x2, #2, ne
cset x4, hs
cset x5, eq
movn w6, #0
cmp w6, #0
ccmn w6, #1, #0, ne
cset x7, eq
cset x8, hs
ccmp w6, w2, #8, eq
cset x9, mi
cset x10, lt
cmp x2, x1
ccmn x2, x1, #15, gt
cset x11, vs
cset x12, gt
ret
