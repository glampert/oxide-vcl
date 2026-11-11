.init_vf_all
--enter
in_vf src(vf01)
--endenter
add.xyzw caty, src, src
.syntax new
add res, caty, src
--exit
out_vf res(vf02)
--endexit
