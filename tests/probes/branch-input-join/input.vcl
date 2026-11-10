.init_vf_all
.init_vi_all
.name join
--enter
	in_vf vf01(vf01)
	in_vi vi01(vi01)
--endenter
	add.xyz vf03, vf01, vf01
	ibeq vi01, vi00, L_one
	mul.y vf03, vf03, vf01
	b L_two
L_one:
	mulw.y vf03, vf03, vf00w
L_two:
	sq.xyz vf03, 0(vi00)
--exit
--endexit
