.init_vf_all
.init_vi_all
--enter
in_vi count(vi01)
in_vi addr(vi02)
in_vf scale(vf01)
--endenter
loop:
--LoopCS 4, 1
lq vec, 0(addr)
mul vec, vec, scale
sq vec, 0(addr)
iaddiu addr, addr, 1
iaddi count, count, -1
ibne count, vi00, loop
--exit
--endexit
