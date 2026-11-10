.init_vi_all
--enter
in_vi counter(vi01)
--endenter
.vsm
nop isubiu counter,counter,1
.endvsm
ibeq counter,vi00,taken
isubiu counter,counter,1
taken:
--exit
out_vi counter(vi01)
--endexit
