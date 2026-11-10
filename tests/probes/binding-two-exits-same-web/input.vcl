.init_vi_all
--enter
in_vi p(vi01)
--endenter
iaddiu t,vi00,5
ibeq p,vi00,other
--exit
out_vi t(vi03)
--endexit
other:
--exit
out_vi t(vi04)
--endexit
