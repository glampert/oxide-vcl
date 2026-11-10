.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
in_vi flag(vi02)
--endenter
mul result,source,source
iaddiu pointer,base,3
ibeq flag,vi00,taken
iaddiu pointer,base,6
taken:
--exit
out_vf result(vf02)
out_vi pointer(vi03)
--endexit
