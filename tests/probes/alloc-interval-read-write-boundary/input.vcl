.init_vf vf03
--enter
in_vf a(vf01)
in_vf b(vf04)
--endenter
.vsm
add temp_one,a,a nop
nop nop
nop nop
nop nop
add first,temp_one,a move temp_two,b
add second,temp_two,a nop
.endvsm
--exit
out_vf first(vf02)
out_vf second(vf05)
--endexit
