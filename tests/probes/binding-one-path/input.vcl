.init_vi_all
--enter
in_vi p(vi01)
in_vi t(vi02)
--endenter
ibeq p,vi00,skip
iaddiu t,vi00,5
skip:
--exit
out_vi t(vi03)
--endexit
