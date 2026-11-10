; oxide-vcl corpus program openvcl__repros_ilw_branch_label
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/ilw_branch_label.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_ilw_branch_label
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    ilw.x iA, 0(vi00)
    ilw.y iB, 0(vi00)
    ibeq iA, vi00, lX
    ilw.z iC, 0(vi00)
    b lJ
lX:
    iaddi iC, iB, 1
lJ:
    ibne iC, vi00, lOut
    isw.x iC, 3(vi00)
lOut:
    isw.y iC, 3(vi00)
--exit
--endexit
