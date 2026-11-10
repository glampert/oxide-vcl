; An FSAND at the top of a loop reads the status flags of the FMAC at its bottom.
.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi n(vi01)
--endenter
iaddiu s,vi00,0
add b,a,a
loop:
fsand r,0x2
iadd s,s,r
sub b,b,a
isubiu n,n,1
ibne n,vi00,loop
--exit
out_vi s(vi02)
out_vf b(vf02)
--endexit
