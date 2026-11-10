.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
.vsm
nop iaddiu pointer,base,3
.endvsm
b next
next:
lq result,5(pointer)
--exit
out_vf result(vf04)
--endexit
