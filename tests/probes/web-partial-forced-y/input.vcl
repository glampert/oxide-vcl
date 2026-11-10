.init_vf vf02,vf10.x
--enter
in_vf vf01
--endenter
mul.y vf10,vf01,vf01
add.y vf02,vf10,vf01
--exit
out_vf.y vf02
--endexit
