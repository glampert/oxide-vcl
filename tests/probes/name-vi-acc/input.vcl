.init_vi_all
--enter
in_vi a(vi01)
--endenter
        iadd    Acc, a, a
        iadd    b, Acc, a
--exit
out_vi b(vi02)
--endexit
