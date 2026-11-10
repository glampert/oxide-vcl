.init_vi_all
.init_vf_all
--enter
in_vi target(vi01)
in_vf a(vf01)
--endenter
        jr      target:done
dead:
        add     b, a, a
        b       dead
done:
        mul     c, a, a
--exit
out_vf c(vf02)
--endexit
