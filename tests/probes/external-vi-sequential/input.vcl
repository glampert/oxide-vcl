.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
iaddiu first,base,3
b step
step:
iaddiu second,first,6
b next
next:
iaddiu result,second,9
--exit
out_vi result(vi04)
--endexit
