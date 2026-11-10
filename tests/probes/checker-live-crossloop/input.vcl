; A VI value set in one loop and read in a later sibling loop that does not
; set it, with enough temporaries in the second loop to tempt a reuse.
.init_vi_all
--enter
in_vi n(vi01)
in_vi ptr(vi02)
--endenter
iaddiu m,vi00,2
first:
ilw.x last,0(ptr)
iaddiu ptr,ptr,1
isubiu n,n,1
ibne n,vi00,first
second:
ilw.x t1,0(ptr)
ilw.x t2,1(ptr)
ilw.x t3,2(ptr)
ilw.x t4,3(ptr)
ilw.x t5,4(ptr)
ilw.x t6,5(ptr)
iadd s1,t1,t2
iadd s2,t3,t4
iadd s3,t5,t6
iadd s1,s1,s2
iadd s1,s1,s3
iadd s1,s1,last
isw.x s1,6(ptr)
iaddiu ptr,ptr,8
isubiu m,m,1
ibne m,vi00,second
--exit
out_vi ptr(vi02)
--endexit
