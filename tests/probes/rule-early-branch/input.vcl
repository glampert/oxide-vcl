.init_vi_all
.init_vf_all
--enter
in_vi base(vi01)
in_vf a(vf01)
--endenter
        ilw.x   n, 0(base)
        iaddiu  m, n, 2
        ibeq    m, vi00, skip
        add     b, a, a
        b       done
skip:
        mul     b, a, a
done:
--exit
out_vf b(vf02)
--endexit
