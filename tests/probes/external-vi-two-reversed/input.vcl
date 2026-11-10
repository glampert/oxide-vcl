.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi base(vi01)
--endenter
iaddiu second,base,6
iaddiu first,base,3
b next
next:
iadd result,first,second
--exit
out_vi result(vi04)
--endexit
