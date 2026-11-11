.init_vf_all
.init_vi_all
--enter
in_vi flag(vi01)
in_vf src(vf01)
--endenter
	ibeq flag, vi00, other
	add res, src, src
--exit
out_vf res(vf02)
--endexit
other:
	sub res, src, src
--exit
out_vf res(vf00)
--endexit
