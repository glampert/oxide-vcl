--enter
in_vf vf01
in_vf vf02
--endenter
.vsm
add vf02,vf01,vf01 mtir vi02,vf02.x
.endvsm
--exit
out_vf vf02
out_vi vi02
--endexit
