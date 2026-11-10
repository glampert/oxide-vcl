--enter
in_vf a(vf01)
--endenter
add vf02,a,a
fsand vi01,0x400
sub vf03,a,a
fsand vi02,0x400
--exit
out_vi vi01
out_vi vi02
--endexit
