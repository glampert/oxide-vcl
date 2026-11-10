.init_vi_all
--enter
in_vi pointer(vi01)
--endenter
.vsm
nop iaddiu pointer,pointer,3
nop lq result,1022(pointer)
.endvsm
--exit
out_vf result(vf02)
--endexit
