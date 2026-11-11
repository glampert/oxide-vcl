.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vf res(vf00)
--endenter
	add res, src, src
--exit
out_vf res
--endexit
