.init_vf_all
.init_vi_all
--enter
in_vi p(vi01)
in_vf v(vf01)
--endenter
lq v,0(p)
add v,v,v
--exit
out_vf v(vf02)
--endexit
