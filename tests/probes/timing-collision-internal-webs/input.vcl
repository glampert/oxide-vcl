.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi count(vi01)
--endenter
add.xyz color,a,a
mfir.w alpha,count
sq.xyz color,0(vi00)
sq.w alpha,1(vi00)
--exit
--endexit
