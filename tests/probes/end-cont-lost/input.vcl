.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
mul first,src,src
add second,src,src
add mid,first,second
--cont
add result,mid,src
--exit
out_vf result(vf04)
--endexit
