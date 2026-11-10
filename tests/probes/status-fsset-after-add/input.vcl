--enter
in_vf a(vf01)
--endenter
add vf02,a,a
fsset 0
fsand vi01,0x400
--exit
out_vi vi01
--endexit
