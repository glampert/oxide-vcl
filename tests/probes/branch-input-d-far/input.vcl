.init_vf_all
.init_vi_all
.name d_far
--enter
	in_vf vf01(vf01)
	in_vi vi01(vi01)
--endenter
	add.xyz vf03, vf01, vf01
	ibeq vi01, vi00, L_one
	sq.xyz vf01, 1(vi00)
	sq.xyz vf01, 2(vi00)
	sq.xyz vf01, 3(vi00)
	sq.xyz vf01, 4(vi00)
	sq.xyz vf03, 5(vi00)
L_one:
	sq.xyz vf03, 0(vi00)
--exit
--endexit
