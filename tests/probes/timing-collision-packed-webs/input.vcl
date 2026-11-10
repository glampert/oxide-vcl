.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi count(vi01)
--endenter
add.xyz color,a,a
mfir.w alpha,count
--exit
out_vf.xyz color(vf02)
out_vf.w alpha(vf03)
--endexit
