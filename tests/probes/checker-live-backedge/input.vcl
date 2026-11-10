; A VI value last written mid-loop and read again above that write on the
; next iteration, with temporaries after the write.
.init_vi_all
--enter
in_vi n(vi01)
in_vi ptr(vi02)
--endenter
iaddiu prev,vi00,0
loop:
ilw.x cur,0(ptr)
iadd d,cur,prev
isw.x d,1(ptr)
iaddiu prev,cur,1
ilw.x u1,2(ptr)
ilw.x u2,3(ptr)
ilw.x u3,4(ptr)
ilw.x u4,5(ptr)
iadd s1,u1,u2
iadd s2,u3,u4
iadd s1,s1,s2
isw.x s1,6(ptr)
iaddiu ptr,ptr,8
isubiu n,n,1
ibne n,vi00,loop
--exit
out_vi ptr(vi02)
--endexit
