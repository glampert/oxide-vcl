; A subroutine ends with an FMAC; the caller reads the MAC flags at the return.
.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi m(vi02)
--endenter
bal vi15,subroutine
back:
fmand r,m
--exit
out_vi r(vi03)
out_vf b(vf02)
--endexit
subroutine:
add b,a,a
jr vi15
