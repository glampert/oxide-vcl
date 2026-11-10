.init_vf_all
.init_vi_all
.name c_coal
--enter
	in_vf vf03(vf03)
	in_vi vi02(vi02)
--endenter
	sq.xyz vf03, 1(vi00)
	isw.x vi02, 2(vi00)
	iaddiu vi02, vi02, 1
	ilw.x vi03, 0(vi02)
	isw.x vi03, 5(vi00)
	sq.xyz vf03, 0(vi00)
	isw.x vi02, 3(vi00)
--exit
--endexit
