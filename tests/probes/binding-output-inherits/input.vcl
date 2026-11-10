.init_vi_all
--enter
in_vi p(vi01)
--endenter
iaddiu t,p,1
--exit
out_vi t(vi03)
--endexit
--enter
in_vi p(vi01)
--endenter
iaddiu t,p,2
--exit
out_vi t
--endexit
