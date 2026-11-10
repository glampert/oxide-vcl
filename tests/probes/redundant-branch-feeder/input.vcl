.init_vf_all
.init_vi_all
.name b
--enter
	in_vf vf01(vf01)
	in_vi vi02(vi02)
--endenter
	add.xyz vf03, vf01, vf01
	iaddiu vi04, vi00, 1
	iand vi04, vi02, vi04
	ibeq vi04, vi00, L_next
L_next:
	sq.xyz vf03, 0(vi00)
--exit
--endexit
