.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
.vsm reorder
add.xyz result,source,vf00 nop
.endvsm
--exit
out_vf.xyz result(vf02)
--endexit
