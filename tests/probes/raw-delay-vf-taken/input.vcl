--enter
in_vf vf01
--endenter
.vsm
nop b target
add vf02,vf01,vf01 nop
target:
add vf03,vf02,vf01 nop
.endvsm
--exit
out_vf vf03
--endexit
