.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
in_vi flag(vi02)
--endenter
iaddiu pointer,base,3
ibeq flag,vi00,joined
iaddiu pointer,base,6
joined:
lq result,5(pointer)
--exit
out_vf result(vf02)
--endexit
