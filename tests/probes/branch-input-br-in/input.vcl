.init_vf_all
.init_vi_all
.name br_in
--enter
	in_vf vf01(vf01)
	in_vi vi01(vi01)
--endenter
	add.xyz vf03, vf01, vf01
	ibeq vi01, vi00, L_one
	sq.xyz vf03, 1(vi00)
L_one:
	sq.xyz vf03, 0(vi00)
--exit
--endexit
