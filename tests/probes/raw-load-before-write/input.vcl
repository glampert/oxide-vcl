--enter
in_vf vf02
in_vi vi01
--endenter
.vsm
add vf01,vf02,vf02 lq vf02,0(vi01)
.endvsm
--exit
out_vf vf01
out_vf vf02
--endexit
