.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
	move res, src
--exit
out_vf res(vf02)
--endexit
--enter
in_vf src(vf01)
in_vf res(vf00)
--endenter
	add other, src, src
--exit
out_vf other(vf02)
--endexit
