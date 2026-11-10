--enter
in_vf vf01
in_vi vi01
--endenter
.vsm
nop ibeq vi01,vi00,target
add vf02,vf01,vf01 nop
.endvsm
add vf03,vf02,vf01
b done
target:
sub vf03,vf02,vf01
done:
--exit
out_vf vf03
--endexit
