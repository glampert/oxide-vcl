.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
.vsm reorder
nop iaddiu vi00,base,0
.endvsm
--exit
out_vi base(vi01)
--endexit
