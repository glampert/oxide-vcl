.init_vf_all
.init_vi vi02
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
iaddiu first,base,3
iaddiu second,base,6
b next
next:
iadd result,first,second
--exit
out_vi result(vi04)
--endexit
