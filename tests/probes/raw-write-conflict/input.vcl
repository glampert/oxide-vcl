--enter
in_vf vf01
in_vi vi01
--endenter
.vsm
add vf02,vf01,vf01 lq vf02,0(vi01)
.endvsm
--exit
out_vf vf02
--endexit
