.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
	add res, src, src
--exit
out_vf res(vf00)
--endexit
--enter
in_vf src(vf01)
--endenter
	move res, src
--exit
out_vf res
--endexit
