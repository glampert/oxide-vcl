; The same IADDI immediates inside a raw block.
--enter
in_vi vi01
--endenter
.vsm
nop iaddi vi02,vi01,-18
nop iaddi vi03,vi01,100
.endvsm
--exit
out_vi vi02
out_vi vi03
--endexit
