.syntax new
.init_vf_all
--enter
in_vf src(vf01)
--endenter
add.y res, src.y, src.y
--exit
out_vf.y res(vf02)
--endexit
