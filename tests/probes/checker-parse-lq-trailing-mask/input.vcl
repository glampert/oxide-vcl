.init_vf_all
.init_vi_all
--enter
in_vi p(vi01)
--endenter
lq a,0(p)x
move.x b,a
mfir.x t,vi00
--exit
out_vi t(vi02)
--endexit
