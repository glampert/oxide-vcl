; An integer setter just before a join; the joined block starts with a
; conditional branch on the same register.
--enter
in_vi a(vi01)
in_vi b(vi02)
--endenter
ibeq a,vi00,join
iaddiu b,b,1
join:
ibne b,vi00,done
iaddiu a,a,2
done:
--exit
out_vi a(vi01)
out_vi b(vi02)
--endexit
