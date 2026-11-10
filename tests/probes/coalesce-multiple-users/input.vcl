.init_vf_all
.init_vi_all
--enter
in_vf source(vf01)
in_vi base(vi01)
--endenter
iaddiu pointer,base,3
lq first,5(pointer)
lq second,6(pointer)
--exit
out_vf first(vf02)
out_vf second(vf03)
--endexit
