.init_vi_all
--enter
in_vi p(vi01)
in_vi t(vi02)
--endenter
ilw.x t,10(p)
--exit
out_vi t(vi03)
--endexit
--enter
in_vi t
--endenter
iaddiu t,t,1
--exit
out_vi t(vi05)
--endexit
