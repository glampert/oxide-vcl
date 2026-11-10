.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
add.xyz vf02,source,vf00
fmand result,base
--exit
out_vi result(vi03)
--endexit
