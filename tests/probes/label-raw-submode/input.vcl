.init_vi_all
.init_vf_all
--enter
in_vi vi01
--endenter
.vsm
nop iaddiu vi01,vi01,1
.endvsm
.vsm reorder
nop iaddiu vi01,vi01,2
.endvsm
.vsm
nop iaddiu vi01,vi01,3
.endvsm
--exit
out_vi vi01
--endexit
--enter
in_vi p(vi01)
--endenter
iaddiu p,p,1
--exit
out_vi p(vi01)
--endexit
