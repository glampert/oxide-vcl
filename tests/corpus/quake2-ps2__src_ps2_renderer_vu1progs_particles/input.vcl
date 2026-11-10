; oxide-vcl corpus program quake2-ps2__src_ps2_renderer_vu1progs_particles
; From https://github.com/glampert/quake2-ps2 at 12f2f9a: src/ps2/renderer/vu1progs/particles.vcl
; Produced: vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j
; License: GPL-2.0, see tests/corpus/licenses/GPL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --

.name VU1Prog_Particles

.init_vi VI02, VI03, VI04, VI05, VI06, VI07, VI08, VI09, VI10, VI11, VI12, VI13, VI14
.init_vf_all
.syntax new
.vu

--enter
--endenter

    fcset 0x000000
    lq fMVP0,      0(vi00)
    lq fMVP1,      1(vi00)
    lq fMVP2,      2(vi00)
    lq fMVP3,      3(vi00)
    lq fGSScale,   4(vi00)
    lq fGSOffset,  5(vi00)
    lq fClipScale, 6(vi00)

    iaddiu iRegRGBAQ, vi00, 1
    xtop   iBase
    ilw.w  iNumPrts,  0(iBase)
    lq     fQuadOff,  1(iBase)
    lq     fUV0,      2(iBase)
    lq     fUV1,      3(iBase)
    iaddiu iInPtr,    iBase, 11
    iadd   iKick, iInPtr, iNumPrts

    iaddiu iTagPtr, iBase, 4
    iaddiu iOutPtr, iKick, 0
    lqi fTag0, (iTagPtr++)
    lqi fTag1, (iTagPtr++)
    lqi fTag2, (iTagPtr++)
    lqi fTag3, (iTagPtr++)
    lqi fTag4, (iTagPtr++)
    lqi fTag5, (iTagPtr++)
    lqi fTag6, (iTagPtr++)
    sqi fTag0, (iOutPtr++)
    sqi fTag1, (iOutPtr++)
    sqi fTag2, (iOutPtr++)
    sqi fTag3, (iOutPtr++)
    sqi fTag4, (iOutPtr++)
    sqi fTag5, (iOutPtr++)
    sqi fTag6, (iOutPtr++)

    lParticleLoop:
        lq fP, 0(iInPtr)
        mul  acc, fMVP0, fP[y]
        madd acc, fMVP1, fP[z]
        madd acc, fMVP2, fP[w]
        madd fC,  fMVP3, vf00[w]
        mul.xyz   fJudge, fC, fClipScale
        clipw.xyz fJudge, fC[w]
        div q, vf00[w], fC[w]
        mul.w  fScale, fC,     fQuadOff[w]
        add.w  fScale, fScale, vf00[w]
        mulq.w   fK,    fScale,   q
        mul.xyz  fOff,  fQuadOff, fK[w]
        mulq.xyz fNdc0, fC,    q
        add.xyz  fNdc1, fNdc0, fOff
        mula.xyz  acc, fGSOffset, vf00[w]
        madd.xyz  fS0, fNdc0, fGSScale
        ftoi4.xyz fS0, fS0
        mula.xyz  acc, fGSOffset, vf00[w]
        madd.xyz  fS1, fNdc1, fGSScale
        ftoi4.xyz fS1, fS1
        addq.y fQ, vf00, q
        fcand  vi01, 0x3F
        iaddiu iADC, vi01, 0x7FFF
        sq.x   fP,        0(iOutPtr)
        sq.y   fQ,        0(iOutPtr)
        isw.z  iRegRGBAQ, 0(iOutPtr)
        sq     fUV0,      1(iOutPtr)
        sq.xyz fS0,       2(iOutPtr)
        isw.w  iADC,      2(iOutPtr)
        sq     fUV1,      3(iOutPtr)
        sq.xyz fS1,       4(iOutPtr)
        isw.w  iADC,      4(iOutPtr)
        iaddiu iInPtr,   iInPtr,   1
        iaddiu iOutPtr,  iOutPtr,  5
        iaddi  iNumPrts, iNumPrts, -1
        ibgtz  iNumPrts, lParticleLoop
    --barrier
    xgkick iKick

--exit
--endexit

