; IADDIU/ISUBIU immediates outside the 15-bit unsigned field (0..32767).
.init_vi_all
--enter
in_vi a(vi01)
--endenter
iaddiu b,a,40000
isubiu c,a,70000
--exit
out_vi b(vi02)
out_vi c(vi03)
--endexit
