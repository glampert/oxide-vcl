.init_vf vf03
--enter
in_vf a(vf01)
in_vf b(vf04)
--endenter
.vsm
add.x temp_one,a,a move.y temp_two,b
nop nop
nop nop
nop nop
add.x result,temp_one,a nop
add.y result,temp_two,a nop
.endvsm
--exit
out_vf.xy result(vf02)
--endexit
