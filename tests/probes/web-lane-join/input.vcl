.init_vf_all
--enter
in_vf vf01
--endenter
mul.x temp,vf01,vf01
sub.y temp,vf01,vf01
add.xy vf02,temp,vf01
--exit
out_vf.xy vf02
--endexit
