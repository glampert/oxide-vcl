; oxide-vcl corpus program openvcl__repros_masked_write_branch
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/masked_write_branch.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_masked_write_branch
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    lq fX, 1(vi00)
    ilw.x iC, 0(vi00)
    move vCol, vf00
    ibeq iC, vi00, lSkip
    move.xyz vCol, vf00
    move.w vCol, fX
lSkip:
    sq vCol, 2(vi00)
--exit
--endexit
