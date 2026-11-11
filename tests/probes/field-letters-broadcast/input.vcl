.init_vf_all
--enter
in_vf src(vf01)
in_vf scale(vf02)
--endenter
mulx res, src, scalex
--exit
out_vf res(vf03)
--endexit
