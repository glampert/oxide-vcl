.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
iaddiu[D] result,base,0
--exit
out_vi result(vi03)
--endexit
