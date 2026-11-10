; An outer loop counter live across an inner loop full of temporaries.
.init_vi_all
--enter
in_vi n(vi01)
in_vi ptr(vi02)
--endenter
outer:
iaddiu j,vi00,3
inner:
ilw.x t1,0(ptr)
ilw.x t2,1(ptr)
ilw.x t3,2(ptr)
ilw.x t4,3(ptr)
ilw.x t5,4(ptr)
iadd s1,t1,t2
iadd s2,t3,t4
iadd s1,s1,s2
iadd s1,s1,t5
iadd s1,s1,n
isw.x s1,5(ptr)
iaddiu ptr,ptr,6
isubiu j,j,1
ibne j,vi00,inner
isubiu n,n,1
ibne n,vi00,outer
--exit
out_vi ptr(vi02)
--endexit
