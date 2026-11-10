--enter
in_vf vf01
in_vi vi01
--endenter
.vsm
add vf02,vf01,vf01 iaddiu vi02,vi01,1
.endvsm
--exit
out_vf vf02
out_vi vi02
--endexit
