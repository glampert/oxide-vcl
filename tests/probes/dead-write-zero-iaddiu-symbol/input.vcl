.init_vf_all
.init_vi_all
--enter
in_vi src(vi01)
--endenter
	iaddiu res, src, SYMBOL
--exit
out_vi res(vi00)
--endexit
