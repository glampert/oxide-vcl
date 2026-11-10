.init_vf_all
.init_vi_all
--enter
	in_vi vi02(vi02)
	in_vf vf10(vf10)
--endenter
	iaddiu vi03, vi02, 0xe1
	lq.xyzw vf01, 99(vi00)
	lq.xyzw vf02, 100(vi00)
	lq.xyzw vf03, 101(vi00)
	lq.xyzw vf04, 102(vi00)
	xgkick vi03
--exit
	out_vf vf01(vf01)
	out_vf vf02(vf02)
	out_vf vf03(vf03)
	out_vf vf04(vf04)
--endexit
