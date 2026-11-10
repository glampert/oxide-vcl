; oxide-vcl corpus program openvcl__repros_mac_join
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/mac_join.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_mac_join
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    ilw.x iFmt, 0(vi00)
    lq fA, 1(vi00)
    ibeq iFmt, vi00, lFmtB
    sub.xyz fB, fA, vf00
    b lJoin
lFmtB:
    add.xyz fB, fA, vf00
lJoin:
    fmand iM, iFmt
    isw.x iM, 2(vi00)
    sq.xyz fB, 3(vi00)
--exit
--endexit
