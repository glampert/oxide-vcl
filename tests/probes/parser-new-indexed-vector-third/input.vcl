.init_vf_all
.init_vi_all
.syntax new
--enter
in_vf model[0](vf01)
in_vf model[1](vf03)
in_vf result[0](vf02)
--endenter
add.x result[0],model[0],model[1]
--exit
out_vf result[0](vf02)
--endexit
