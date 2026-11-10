; A loop whose last integer write feeds the conditional branch at its top.
.init_vi_all
--enter
in_vi n(vi01)
in_vi ptr(vi02)
--endenter
top:
ibeq n,vi00,done
ilw.x t,0(ptr)
iadd t,t,n
isw.x t,0(ptr)
iaddiu ptr,ptr,1
isubiu n,n,1
b top
done:
--exit
out_vi ptr(vi02)
--endexit
