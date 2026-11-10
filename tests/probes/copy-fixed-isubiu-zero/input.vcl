.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
.vsm
nop isubiu result,base,0
.endvsm
--exit
out_vi result(vi03)
--endexit
