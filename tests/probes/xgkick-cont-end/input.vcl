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
--cont
	mulax ACC, vf01, vf10x
	madday ACC, vf02, vf10y
	maddaz ACC, vf03, vf10z
	maddw vf11, vf04, vf10w
	sq vf11, 0(vi00)
--exit
--endexit
