.init_vi_all
--enter
in_vi source(vi01)
--endenter
.vsm
nop iaddiu temp_one,source,5
nop isubiu result,temp_one,2
.endvsm
--exit
out_vi result(vi03)
--endexit
