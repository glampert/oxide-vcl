.syntax new
.init_vf_all
--enter
in_vf src(vf01)
--endenter
add.xyz res, src, src
--exit
out_vf res.xyz(vf02)
--endexit
