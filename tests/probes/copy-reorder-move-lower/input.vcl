.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
.vsm reorder
nop move result,source
.endvsm
--exit
out_vf result(vf02)
--endexit
