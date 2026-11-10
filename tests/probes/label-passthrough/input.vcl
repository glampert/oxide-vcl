.init_vi_all
.init_vf_all
--enter
in_vi p(vi01)
--endenter
iaddiu p,p,1
--exit
out_vi p(vi01)
--endexit
----*
.text
.globl example
*----
--enter
in_vi p(vi01)
--endenter
iaddiu p,p,1
--exit
out_vi p(vi01)
--endexit
