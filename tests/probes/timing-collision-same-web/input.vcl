.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi count(vi01)
--endenter
add.xyz joined,a,a
mfir.w joined,count
--exit
out_vf joined(vf02)
--endexit
