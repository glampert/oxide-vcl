.init_vi_all
--enter
in_vi p(vi01)
in_vi t(vi02)
--endenter
iaddiu n,vi00,4
top:
iaddiu t,t,1
iaddi n,n,-1
ibne n,vi00,top
iaddiu t,vi00,9
--exit
out_vi t(vi03)
--endexit
