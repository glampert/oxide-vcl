.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi addr(vi01)
--endenter
lq.xyz val, 0(addr)
mul.xyz prod, val, src
add.xyz total, prod, src
sq.xyz total, 1(addr)
iaddiu addr, addr, 2
--exit
out_vi addr(vi01)
--endexit
