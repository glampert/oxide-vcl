.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
	add tmp, src, src
	mul res, tmp, tmp
--exit
out_vf res(vf00)
--endexit
