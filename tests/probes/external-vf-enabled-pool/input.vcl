.init_vf vf03
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
mul temp,src,src
b next
next:
add result,temp,temp
--exit
out_vf result(vf02)
--endexit
