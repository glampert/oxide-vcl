.syntax new
.init_vf_all
--enter
in_vf src.xyz(vf01)
--endenter
add.xyz res, src, src
--exit
out_vf.xyz res(vf02)
--endexit
