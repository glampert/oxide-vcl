.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
	add res, src, src
	add other, res, res
--exit
out_vf res(vf00)
out_vf other(vf02)
--endexit
