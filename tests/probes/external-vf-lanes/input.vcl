.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
mul.x left,src,src
mul.y right,src,src
b next
next:
add.x result,left,left
add.y result,right,right
--exit
out_vf.xy result(vf04)
--endexit
