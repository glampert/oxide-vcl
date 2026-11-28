.syntax new
.init_vf_all
.init_vi_all
--enter
--endenter
        lq.yz      a, 0(vi00)
        sub.z      diff, a, vf00[x]
        move.y     kept, a
        fsand      flag, 3
        ibne       flag, vi00, skip
        sub.y      kept, vf00, a[y]
skip:
        sq.y       kept, 1(vi00)
--exit
--endexit
