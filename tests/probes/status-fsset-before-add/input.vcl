--enter
in_vf a(vf01)
--endenter
fsset 0
add vf02,a,a
fsand vi01,0x400
--exit
out_vi vi01
--endexit
