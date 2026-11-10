.init_vf vf03
--enter
in_vf a(vf01)
in_vf b(vf02)
--endenter
.vsm
add.x temp_one,a,b nop
mul.x temp_two,a,b nop
nop nop
nop nop
add.x result,temp_one,a nop
addx.y result,b,temp_two.x nop
.endvsm
--exit
out_vf.xy result(vf04)
--endexit
