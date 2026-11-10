.init_vi_all
--enter
in_vi a(vi01)
--endenter
        iadd    R, a, a
        iadd    b, R, a
--exit
out_vi b(vi02)
--endexit
