--enter
in_vi vi01
--endenter
.vsm
nop b target
nop iaddiu vi02,vi01,3
target:
nop iaddiu vi03,vi02,4
.endvsm
--exit
out_vi vi03
--endexit
