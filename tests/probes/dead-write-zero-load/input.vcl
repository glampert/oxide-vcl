.init_vf_all
.init_vi_all
--enter
in_vi base(vi01)
--endenter
	lq res, 0(base)
--exit
out_vf res(vf00)
--endexit
