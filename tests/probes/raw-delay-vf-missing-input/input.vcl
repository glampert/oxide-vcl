--enter
in_vi vi01
--endenter
.vsm
nop ibeq vi01,vi00,target
add vf02,vf01,vf01 nop
target:
add vf03,vf02,vf01 nop
.endvsm
--exit
out_vf vf03
--endexit
