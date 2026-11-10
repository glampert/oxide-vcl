; Memory displacements outside the 11-bit signed field (-1024..1023).
.init_vf_all
.init_vi_all
--enter
in_vi p(vi01)
--endenter
lq a,1024(p)
sq a,-1025(p)
ilw.x i,2047(p)
isw.x i,-2000(p)
--exit
--endexit
