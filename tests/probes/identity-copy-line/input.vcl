.init_vf_all
.init_vi_all
.name line
--enter
	in_vf vf01(vf01)
--endenter
	add.xyz vf03, vf01, vf01
	sq.xyz vf03, 1(vi00)
	mulw.y vf03, vf03, vf00w
	sq.xyz vf03, 0(vi00)
--exit
--endexit
