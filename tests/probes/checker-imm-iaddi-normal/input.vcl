; IADDI immediates outside the 5-bit signed field (-16..15), scheduled code.
.init_vi_all
--enter
in_vi a(vi01)
--endenter
iaddi b,a,-18
iaddi c,a,100
--exit
out_vi b(vi02)
out_vi c(vi03)
--endexit
