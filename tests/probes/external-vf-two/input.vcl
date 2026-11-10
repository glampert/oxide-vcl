.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
mul first,src,src
add second,src,src
b next
next:
add result,first,second
--exit
out_vf result(vf04)
--endexit
