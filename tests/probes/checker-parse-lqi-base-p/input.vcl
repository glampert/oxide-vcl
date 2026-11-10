.init_vf_all
.init_vi_all
--enter
in_vi p(vi01)
--endenter
lqi a,(p++)
mtir t,a.x
--exit
out_vi t(vi02)
--endexit
