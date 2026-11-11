.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi cnt(vi01)
--endenter
top:
	add res, src, src
	iaddi cnt, cnt, -1
	ibne cnt, vi00, top
--exit
out_vf res(vf00)
--endexit
