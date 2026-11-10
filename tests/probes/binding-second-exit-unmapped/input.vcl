.init_vi_all
--enter
in_vi p(vi01)
in_vi t(vi02)
--endenter
ibeq p,vi00,other
iaddiu t,vi00,5
--exit
out_vi t(vi03)
--endexit
other:
iaddiu t,vi00,7
--exit
out_vi t
--endexit
