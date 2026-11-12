.init_vf vf01-vf04
--enter
in_vf src(vf10)
--endenter
add.xyzw ta, src, src
mul.xyzw tb, src, src
sub.xyzw tc, src, vf00
add.xyzw td, ta, src
mul.xyzw te, tb, src
add.xyzw tf, tc, ta
add.xyzw sum1, ta, tb
add.xyzw sum2, tc, td
add.xyzw sum3, te, tf
add.xyzw sum4, sum1, sum2
add.xyzw res, sum4, sum3
--exit
out_vf res(vf11)
--endexit
