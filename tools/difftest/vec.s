// Advanced SIMD three-same integer and bitwise lane operations.
movz x1, #0x1234
movk x1, #0xfedc, lsl #16
movk x1, #0x8001, lsl #48
dup v0.4s, w1
movi v1.16b, #0x0f
fmov d2, x1
mov v2.d[1], x1
add v16.2d, v0.2d, v2.2d
add v17.4s, v0.4s, v1.4s
sub v18.8h, v1.8h, v0.8h
mul v19.4s, v0.4s, v1.4s
and v20.16b, v0.16b, v1.16b
bic v21.16b, v0.16b, v1.16b
orn v22.8b, v0.8b, v1.8b
eor v23.16b, v0.16b, v2.16b
mov v24.16b, v0.16b
bsl v24.16b, v1.16b, v2.16b
cmeq v25.4s, v0.4s, v17.4s
cmgt v26.8h, v18.8h, v1.8h
cmhi v27.16b, v0.16b, v1.16b
umax v28.4s, v0.4s, v18.4s
smin v29.8h, v18.8h, v0.8h
mov v3.16b, v2.16b
bit v3.16b, v1.16b, v0.16b
mov v4.16b, v2.16b
bif v4.8b, v1.8b, v0.8b
cmtst v5.2d, v2.2d, v1.2d
cmhs v6.4s, v18.4s, v0.4s
cmge v7.16b, v18.16b, v2.16b
ret
