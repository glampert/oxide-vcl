.init_vf vf02,vf10.x
--enter
in_vf vf01
--endenter
mul.x vf10,vf01,vf01
add.x vf02,vf10,vf01
--exit
out_vf.x vf02
--endexit
