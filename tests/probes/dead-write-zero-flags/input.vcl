.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
	add res, src, src
	fmand flag, vi00
--exit
out_vf res(vf00)
out_vi flag(vi02)
--endexit
