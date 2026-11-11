.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vf res(vf02)
--endenter
	add other, res, res
	add res, src, src
--exit
out_vf res(vf00)
out_vf other(vf03)
--endexit
