.init_vi_all
--enter
in_vi counter(vi01)
--endenter
.vsm
nop isubiu counter,counter,1
nop isubiu counter,counter,1
.endvsm
--exit
out_vi counter(vi01)
--endexit
