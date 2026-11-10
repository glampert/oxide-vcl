.init_vf_all
.init_vi_all
--enter
	in_vi vi01(vi01)
	in_vi vi02(vi02)
--endenter
	lq.xyzw vf05, 8(vi01)
	lq.xyzw vf06, 9(vi01)
	sq.xyzw vf05, 10(vi00)
	sq.xyzw vf06, 11(vi00)
	xgkick vi00
	lq.xyzw vf07, 12(vi01)
	sq.xyzw vf07, 13(vi01)
	xgkick vi02
	lq.xyzw vf08, 14(vi01)
	sq.xyzw vf08, 15(vi01)
--exit
--endexit
