.init_vi_all
--enter
in_vi a(vi01)
--endenter
        iadd    stat_alu, a, a
        iadd    b, stat_alu, a
--exit
out_vi b(vi02)
--endexit
