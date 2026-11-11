.init_vf_all
.init_vi_all
--enter
in_vi flag(vi01)
in_vf src(vf01)
--endenter
	move res, src
	ibeq flag, vi00, other
--exit
out_vf res(vf02)
--endexit
other:
--exit
out_vf res(vf00)
--endexit
