; An FMAC result's MAC flags read by FMAND after a join.
--enter
in_vf a(vf01)
in_vi m(vi02)
in_vi c(vi03)
--endenter
add b,a,a
ibeq c,vi00,skip
iaddiu c,c,1
skip:
fmand vi01,m
--exit
out_vi vi01
out_vi c(vi03)
out_vf b(vf02)
--endexit
