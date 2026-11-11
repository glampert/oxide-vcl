.syntax new
.init_vf_all
--enter
in_vf src(vf01)
in_vf scale(vf02)
--endenter
add res, src, scale[x]
--exit
out_vf res(vf03)
--endexit
