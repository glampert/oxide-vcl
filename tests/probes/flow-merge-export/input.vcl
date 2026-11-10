.global next
.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
in_vi flag(vi02)
--endenter
iaddiu pointer,base,3
b next
next:
lq result,5(pointer)
--exit
out_vf result(vf02)
out_vi pointer(vi03)
--endexit
