; oxide-vcl corpus program quake2-ps2__src_ps2_renderer_vu1progs_textured_triangles
; From https://github.com/glampert/quake2-ps2 at 12f2f9a: src/ps2/renderer/vu1progs/textured_triangles.vcl
; Produced: vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j
; License: GPL-2.0, see tests/corpus/licenses/GPL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --

.name VU1Prog_TexturedTriangles

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
    sub.x fMinusOne, vf00, vf00[w]
    xtop   iBase
    ilw.w  iNumVerts,  0(iBase)
    ilw.x  iColorMode, 0(iBase)
    iaddiu iInPtr, iBase, 9

    iaddiu iWin,    iBase, 189
    iaddiu iDelta,  vi00,  142
    iaddiu iTagPtr, iBase, 2
    isw.x iWin,    1008(vi00)
    isw.y iDelta,  1008(vi00)
    isw.z iTagPtr, 1008(vi00)
    --barrier

    ilw.y  iWarp, 0(iBase)
    isw.w  iWarp, 1008(vi00)
    ilw.z  iFormat, 0(iBase)
    iaddiu iStride, vi00, 6
    ibeq   iFormat, vi00, lStrideSet
    iaddiu iStride, vi00, 9
    lStrideSet:
    isw.y  iStride, 1009(vi00)
    --barrier

    ilw.x iOutPtr, 1008(vi00)
    ilw.z iTagPtr, 1008(vi00)
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
    iaddiu iVertsLeft, vi00, 45

    lTriangleLoop:
        lq fLightX,     1010 + 0(vi00)
        lq fLightY,     1010 + 1(vi00)
        lq fLightZ,     1010 + 2(vi00)
        lq fNegDiv0,    1010 + 3(vi00)
        lq fNegDiv1,    1010 + 4(vi00)
        lq fNegDiv2,    1010 + 5(vi00)
        lq fNegDiv3,    1010 + 6(vi00)
        lq fColour0,    1010 + 7(vi00)
        lq fColour1,    1010 + 8(vi00)
        lq fColour2,    1010 + 9(vi00)
        lq fColour3,    1010 + 10(vi00)
        lq fLightClamp, 1010 + 11(vi00)
        iaddiu iRoom, vi00,       18
        isub   iRoom, iVertsLeft, iRoom
        ibgez iRoom, lWindowHasRoom

    iaddiu iNloop, vi00,   45
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lKicked1
    ilw.x  iWin,   1008(vi00)
    ilw.y  iDelta, 1008(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 6(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    1008(vi00)
    isw.y  iDelta,  1008(vi00)
    --barrier
    xgkick iKickAt
    lKicked1:

    ilw.x iOutPtr, 1008(vi00)
    ilw.z iTagPtr, 1008(vi00)
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
    iaddiu iVertsLeft, vi00, 45

        lWindowHasRoom:
        ilw.z iFormat, 0(iBase)
        ibne  iFormat, vi00, lKeyframes

    lq fPos0, 0(iInPtr)
    lq fStq0, 1(iInPtr)
    move.xyz fCol0, vf00
    move.w   fCol0, fLightClamp
    ibeq iColorMode, vi00, lLight0Done
    sub fLx, fLightX, fPos0[x]
    sub fLy, fLightY, fPos0[y]
    sub fLz, fLightZ, fPos0[z]
    mul  acc,   fLx, fLx
    madd acc,   fLy, fLy
    madd fDist, fLz, fLz
    mula.xyz acc,   fNegDiv0, fDist[x]
    madd.xyz fTerm, fColour0, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol0,  fCol0,     fTerm
    mula.xyz acc,   fNegDiv1, fDist[y]
    madd.xyz fTerm, fColour1, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol0,  fCol0,     fTerm
    mula.xyz acc,   fNegDiv2, fDist[z]
    madd.xyz fTerm, fColour2, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol0,  fCol0,     fTerm
    mula.xyz acc,   fNegDiv3, fDist[w]
    madd.xyz fTerm, fColour3, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol0,  fCol0,     fTerm
    mini.xyz fCol0, fCol0, fLightClamp
    lLight0Done:
    mul  acc,  fMVP0, fPos0[x]
    madd acc,  fMVP1, fPos0[y]
    madd acc,  fMVP2, fPos0[z]
    madd fPos0, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos0, fClipScale
    clipw.xyz fJudge, fPos0[w]

    lq fPos1, 2(iInPtr)
    lq fStq1, 3(iInPtr)
    move.xyz fCol1, vf00
    move.w   fCol1, fLightClamp
    ibeq iColorMode, vi00, lLight1Done
    sub fLx, fLightX, fPos1[x]
    sub fLy, fLightY, fPos1[y]
    sub fLz, fLightZ, fPos1[z]
    mul  acc,   fLx, fLx
    madd acc,   fLy, fLy
    madd fDist, fLz, fLz
    mula.xyz acc,   fNegDiv0, fDist[x]
    madd.xyz fTerm, fColour0, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol1,  fCol1,     fTerm
    mula.xyz acc,   fNegDiv1, fDist[y]
    madd.xyz fTerm, fColour1, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol1,  fCol1,     fTerm
    mula.xyz acc,   fNegDiv2, fDist[z]
    madd.xyz fTerm, fColour2, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol1,  fCol1,     fTerm
    mula.xyz acc,   fNegDiv3, fDist[w]
    madd.xyz fTerm, fColour3, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol1,  fCol1,     fTerm
    mini.xyz fCol1, fCol1, fLightClamp
    lLight1Done:
    mul  acc,  fMVP0, fPos1[x]
    madd acc,  fMVP1, fPos1[y]
    madd acc,  fMVP2, fPos1[z]
    madd fPos1, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos1, fClipScale
    clipw.xyz fJudge, fPos1[w]

    lq fPos2, 4(iInPtr)
    lq fStq2, 5(iInPtr)
    move.xyz fCol2, vf00
    move.w   fCol2, fLightClamp
    ibeq iColorMode, vi00, lLight2Done
    sub fLx, fLightX, fPos2[x]
    sub fLy, fLightY, fPos2[y]
    sub fLz, fLightZ, fPos2[z]
    mul  acc,   fLx, fLx
    madd acc,   fLy, fLy
    madd fDist, fLz, fLz
    mula.xyz acc,   fNegDiv0, fDist[x]
    madd.xyz fTerm, fColour0, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol2,  fCol2,     fTerm
    mula.xyz acc,   fNegDiv1, fDist[y]
    madd.xyz fTerm, fColour1, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol2,  fCol2,     fTerm
    mula.xyz acc,   fNegDiv2, fDist[z]
    madd.xyz fTerm, fColour2, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol2,  fCol2,     fTerm
    mula.xyz acc,   fNegDiv3, fDist[w]
    madd.xyz fTerm, fColour3, vf00[w]
    max.xyz  fTerm, fTerm,    vf00
    add.xyz  fCol2,  fCol2,     fTerm
    mini.xyz fCol2, fCol2, fLightClamp
    lLight2Done:
    mul  acc,  fMVP0, fPos2[x]
    madd acc,  fMVP1, fPos2[y]
    madd acc,  fMVP2, fPos2[z]
    madd fPos2, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos2, fClipScale
    clipw.xyz fJudge, fPos2[w]

    fcand  vi01, 0x3FFFF
    iaddiu iADC, vi01, 0x7FFF

        b lTransformed
    lKeyframes:
        lq fFrontV,     1010 + 0(vi00)
        lq fBackV,      1010 + 1(vi00)
        lq fShadeLight, 1010 + 2(vi00)
        lq fStScale,    1010 + 3(vi00)
        lq fColorClamp, 7(vi00)

    lq fCurI, 0(iInPtr)
    lq fOldI, 1(iInPtr)
    lq fStq0,  2(iInPtr)
    itof0 fCur, fCurI
    itof0 fOld, fOldI
    mul.yz fStq0, fStq0, fStScale
    mul.xyz  fCol0, fShadeLight, fOld[w]
    move.w   fCol0, fShadeLight
    max.xyz  fCol0, fCol0, vf00
    mini.xyz fCol0, fCol0, fColorClamp
    mul  acc,   fFrontV, fCur
    madd fLerp, fBackV,  fOld
    mul  acc,  fMVP0, fLerp[x]
    madd acc,  fMVP1, fLerp[y]
    madd acc,  fMVP2, fLerp[z]
    madd fPos0, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos0, fClipScale
    clipw.xyz fJudge, fPos0[w]

    lq fCurI, 3(iInPtr)
    lq fOldI, 4(iInPtr)
    lq fStq1,  5(iInPtr)
    itof0 fCur, fCurI
    itof0 fOld, fOldI
    mul.yz fStq1, fStq1, fStScale
    mul.xyz  fCol1, fShadeLight, fOld[w]
    move.w   fCol1, fShadeLight
    max.xyz  fCol1, fCol1, vf00
    mini.xyz fCol1, fCol1, fColorClamp
    mul  acc,   fFrontV, fCur
    madd fLerp, fBackV,  fOld
    mul  acc,  fMVP0, fLerp[x]
    madd acc,  fMVP1, fLerp[y]
    madd acc,  fMVP2, fLerp[z]
    madd fPos1, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos1, fClipScale
    clipw.xyz fJudge, fPos1[w]

    lq fCurI, 6(iInPtr)
    lq fOldI, 7(iInPtr)
    lq fStq2,  8(iInPtr)
    itof0 fCur, fCurI
    itof0 fOld, fOldI
    mul.yz fStq2, fStq2, fStScale
    mul.xyz  fCol2, fShadeLight, fOld[w]
    move.w   fCol2, fShadeLight
    max.xyz  fCol2, fCol2, vf00
    mini.xyz fCol2, fCol2, fColorClamp
    mul  acc,   fFrontV, fCur
    madd fLerp, fBackV,  fOld
    mul  acc,  fMVP0, fLerp[x]
    madd acc,  fMVP1, fLerp[y]
    madd acc,  fMVP2, fLerp[z]
    madd fPos2, fMVP3, vf00[w]
    mul.xyz   fJudge, fPos2, fClipScale
    clipw.xyz fJudge, fPos2[w]

    fcand  vi01, 0x3FFFF
    iaddiu iADC, vi01, 0x7FFF

        sub        fEdge1, fPos1,  fPos0
        sub        fEdge2, fPos2,  fPos0
        add.z      fEdge1, vf00,   fEdge1[w]
        add.z      fEdge2, vf00,   fEdge2[w]
        move.xy    fEye,   fPos0
        add.z      fEye,   vf00,   fPos0[w]
        opmula.xyz acc,    fEdge1, fEdge2
        opmsub.xyz fNorm,  fEdge2, fEdge1
        mul.xyz    fArea,  fNorm,  fEye
        add.x      fArea,  fArea,  fArea[y]
        add.x      fArea,  fArea,  fArea[z]
        mul.x      fArea,  fArea,  fStScale[x]
        mini.x     fArea,  fArea,  vf00[w]
        max.x      fArea,  fArea,  fMinusOne[x]
        ftoi4.x    fArea,  fArea
        mtir       iCull,  fArea[x]
        ibltz      iCull,  lTriangleAdvance
    lTransformed:
        ibne  vi01, vi00, lClipTriangle
        ilw.w iWarp, 1008(vi00)
        ibne  iWarp, vi00, lWarpWhole

    div        q,          vf00[w], fPos0[w]
    mul.xyz    fProj,      fPos0,    q
    mulq       fStqScaled, fStq0,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  0(iOutPtr)
    sq.xyz fProj, 2(iOutPtr)
    ibeq iColorMode, vi00, lEmitUn0Packed
    ftoi0 fRGBA, fCol0
    sq    fRGBA, 1(iOutPtr)
    b lEmitUn0Done
    lEmitUn0Packed:
    sq.x   fStq0, 1(iOutPtr)
    sq.y   fQ,   1(iOutPtr)
    isw.z  iRegRGBAQ, 1(iOutPtr)
    lEmitUn0Done:

    div        q,          vf00[w], fPos1[w]
    mul.xyz    fProj,      fPos1,    q
    mulq       fStqScaled, fStq1,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  3(iOutPtr)
    sq.xyz fProj, 5(iOutPtr)
    ibeq iColorMode, vi00, lEmitUn1Packed
    ftoi0 fRGBA, fCol1
    sq    fRGBA, 4(iOutPtr)
    b lEmitUn1Done
    lEmitUn1Packed:
    sq.x   fStq1, 4(iOutPtr)
    sq.y   fQ,   4(iOutPtr)
    isw.z  iRegRGBAQ, 4(iOutPtr)
    lEmitUn1Done:

    div        q,          vf00[w], fPos2[w]
    mul.xyz    fProj,      fPos2,    q
    mulq       fStqScaled, fStq2,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  6(iOutPtr)
    sq.xyz fProj, 8(iOutPtr)
    ibeq iColorMode, vi00, lEmitUn2Packed
    ftoi0 fRGBA, fCol2
    sq    fRGBA, 7(iOutPtr)
    b lEmitUn2Done
    lEmitUn2Packed:
    sq.x   fStq2, 7(iOutPtr)
    sq.y   fQ,   7(iOutPtr)
    isw.z  iRegRGBAQ, 7(iOutPtr)
    lEmitUn2Done:

    isw.w  iADC, 2(iOutPtr)
    isw.w  iADC, 5(iOutPtr)
    isw.w  iADC, 8(iOutPtr)

        iaddiu iOutPtr,    iOutPtr,     9
        iaddi  iVertsLeft, iVertsLeft, -3
        b lTriangleAdvance
    lWarpWhole:

    iaddiu iWalk, vi00, 981
    sq fPos0, 0(iWalk)
    sq fStq0, 1(iWalk)
    sq fCol0, 2(iWalk)
    sq fPos1, 3(iWalk)
    sq fStq1, 4(iWalk)
    sq fCol1, 5(iWalk)
    sq fPos2, 6(iWalk)
    sq fStq2, 7(iWalk)
    sq fCol2, 8(iWalk)
    sq fPos0, 9(iWalk)
    sq fStq0, 10(iWalk)
    sq fCol0, 11(iWalk)
    iaddiu iCount, vi00, 3
    isw.x  iCount, 1009(vi00)

        --barrier

    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq0
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y    fCutST, vf00, fUV[x]
    add.z    fCutST, vf00, fUV[y]
    ftoi0.yz fShiftI, fCutST
    itof0.yz fShift,  fShiftI
    sub.yz   fCutST,  fCutST, fShift
    sq.yz    fCutST,  981 + 1(vi00)
    sq.yz    fCutST,  981 + 10(vi00)
    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq1
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y  fCutST, vf00,   fUV[x]
    add.z  fCutST, vf00,   fUV[y]
    sub.yz fCutST, fCutST, fShift
    sq.yz  fCutST, 981 + 4(vi00)
    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq2
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y  fCutST, vf00,   fUV[x]
    add.z  fCutST, vf00,   fUV[y]
    sub.yz fCutST, fCutST, fShift
    sq.yz  fCutST, 981 + 7(vi00)

        b lFan
    lClipTriangle:

    iaddiu iWalk, vi00, 957
    sq fPos0, 0(iWalk)
    sq fStq0, 1(iWalk)
    sq fCol0, 2(iWalk)
    sq fPos1, 3(iWalk)
    sq fStq1, 4(iWalk)
    sq fCol1, 5(iWalk)
    sq fPos2, 6(iWalk)
    sq fStq2, 7(iWalk)
    sq fCol2, 8(iWalk)
    sq fPos0, 9(iWalk)
    sq fStq0, 10(iWalk)
    sq fCol0, 11(iWalk)
    iaddiu iCount, vi00, 3
    isw.x  iCount, 1009(vi00)

        ilw.w iWarp, 1008(vi00)
        ibeq  iWarp, vi00, lSeedWarped

    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq0
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y    fCutST, vf00, fUV[x]
    add.z    fCutST, vf00, fUV[y]
    ftoi0.yz fShiftI, fCutST
    itof0.yz fShift,  fShiftI
    sub.yz   fCutST,  fCutST, fShift
    sq.yz    fCutST,  957 + 1(vi00)
    sq.yz    fCutST,  957 + 10(vi00)
    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq1
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y  fCutST, vf00,   fUV[x]
    add.z  fCutST, vf00,   fUV[y]
    sub.yz fCutST, fCutST, fShift
    sq.yz  fCutST, 957 + 4(vi00)
    lq fWarpFold, 1022 + 0(vi00)
    lq fWarpPoly, 1022 + 1(vi00)
    lq fBatch,    1(iBase)
    add fHalf,    vf00, fWarpFold[y]
    sub fNegHalf, vf00, fHalf
    mr32 fRaw, fStq2
    mul.xy fPhase, fRaw,   fWarpFold[z]
    add.xy fPhase, fPhase, fClipScale[w]
    add.xy   fBias,  fPhase, fWarpFold[x]
    ftoi0.xy fTrunc, fBias
    itof0.xy fTrunc, fTrunc
    sub.xy   fFrac,  fBias,  fTrunc
    sub.xy   fFrac,  fFrac,  fWarpFold[y]
    sub.xy  fFoldA, fHalf,    fFrac
    mini.xy fT,     fFrac,    fFoldA
    sub.xy  fFoldB, fNegHalf, fFrac
    max.xy  fT,     fT,       fFoldB
    mul.xy fTSqr, fT,    fT
    mul.xy fPoly, fTSqr, fWarpPoly[w]
    add.xy fPoly, fPoly, fWarpPoly[z]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[y]
    mul.xy fPoly, fPoly, fTSqr
    add.xy fPoly, fPoly, fWarpPoly[x]
    mul.xy fWarp, fPoly, fT
    add.x  fUV, fRaw, fWarp[y]
    add.y  fUV, fRaw, fWarp[x]
    add.x  fUV, fUV,  fBatch[z]
    mul.xy fUV, fUV,  fBatch
    add.y  fCutST, vf00,   fUV[x]
    add.z  fCutST, vf00,   fUV[y]
    sub.yz fCutST, fCutST, fShift
    sq.yz  fCutST, 957 + 7(vi00)

        lSeedWarped:

    ilw.x  iLeft,  1009(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1009(vi00)
    iaddi iSignCur, iLeft, -3
    ibltz iSignCur, lNearOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lNearLoop:
        lq fCurPos, 0(iWalk)
        lq fCurStq, 1(iWalk)
        lq fCurCol, 2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtStq, 4(iWalk)
        lq fNxtCol, 5(iWalk)
        mul.xyz fJpC,  fCurPos, fClipScale
        mul.xyz fJpN,  fNxtPos, fClipScale
        addw.x  fDCur, vf00,    fCurPos
        addw.x  fDNxt, vf00,    fNxtPos
        sub.xyz fJnC,  vf00,    fJpC
        sub.xyz fJnN,  vf00,    fJpN
        mul.x   fDCur, fDCur,   fGSScale[w]
        mul.x   fDNxt, fDNxt,   fGSScale[w]
        add.x   fDCur, fDCur,   fJnC[z]
        add.x   fDNxt, fDNxt,   fJnN[z]
        mul.x   fDCur, fDCur,   fGSOffset[w]
        mul.x   fDNxt, fDNxt,   fGSOffset[w]
        mul.x   fClipP,    fDCur,   fDNxt
        mini.x  fClipT,    fDCur,   vf00[w]
        mini.x  fClipP,    fClipP,  vf00[w]
        max.x   fClipT,    fClipT,  fMinusOne[x]
        max.x   fClipP,    fClipP,  fMinusOne[x]
        ftoi4.x fClipT,    fClipT
        ftoi4.x fClipP,    fClipP
        mtir    iSignCur,  fClipT[x]
        mtir    iSignProd, fClipP[x]
        ibltz iSignCur, lNearKept
        sq fCurPos, 0(iOut)
        sq fCurStq, 1(iOut)
        sq fCurCol, 2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lNearKept:
        ibgez iSignProd, lNearNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutStq, fNxtStq, fCurStq
        sub fCutCol, fNxtCol, fCurCol
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutStq, fCutStq, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutStq, fCurStq, fCutStq
        add fCutCol, fCurCol, fCutCol
        sq fCutPos, 0(iOut)
        sq fCutStq, 1(iOut)
        sq fCutCol, 2(iOut)
        sq.x fCurStq, 1(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lNearNoCut:
        iaddiu iWalk, iWalk, 3
        iaddi  iLeft, iLeft, -1
        ibgtz  iLeft, lNearLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lNearWrap
    iaddiu iCount,   vi00, 0
    lNearWrap:
    ibltz iSignCur, lNearOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurStq, 981 + 1(vi00)
    lq fCurCol, 981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurStq, 1(iOut)
    sq fCurCol, 2(iOut)
    isw.x iCount, 1009(vi00)
    lNearOut:

    ilw.x  iLeft,  1009(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1009(vi00)
    iaddi iSignCur, iLeft, -3
    ibltz iSignCur, lXlOut
    iaddiu iWalk, vi00, 981
    iaddiu iOut,  vi00, 957
    lXlLoop:
        lq fCurPos, 0(iWalk)
        lq fCurStq, 1(iWalk)
        lq fCurCol, 2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtStq, 4(iWalk)
        lq fNxtCol, 5(iWalk)
        mul.xyz fJpC,  fCurPos, fClipScale
        mul.xyz fJpN,  fNxtPos, fClipScale
        addw.x  fDCur, vf00,    fCurPos
        addw.x  fDNxt, vf00,    fNxtPos
        sub.xyz fJnC,  vf00,    fJpC
        sub.xyz fJnN,  vf00,    fJpN
        mul.x   fDCur, fDCur,   fGSScale[w]
        mul.x   fDNxt, fDNxt,   fGSScale[w]
        add.x   fDCur, fDCur,   fJnC[x]
        add.x   fDNxt, fDNxt,   fJnN[x]
        mul.x   fDCur, fDCur,   fGSOffset[w]
        mul.x   fDNxt, fDNxt,   fGSOffset[w]
        mul.x   fClipP,    fDCur,   fDNxt
        mini.x  fClipT,    fDCur,   vf00[w]
        mini.x  fClipP,    fClipP,  vf00[w]
        max.x   fClipT,    fClipT,  fMinusOne[x]
        max.x   fClipP,    fClipP,  fMinusOne[x]
        ftoi4.x fClipT,    fClipT
        ftoi4.x fClipP,    fClipP
        mtir    iSignCur,  fClipT[x]
        mtir    iSignProd, fClipP[x]
        ibltz iSignCur, lXlKept
        sq fCurPos, 0(iOut)
        sq fCurStq, 1(iOut)
        sq fCurCol, 2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXlKept:
        ibgez iSignProd, lXlNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutStq, fNxtStq, fCurStq
        sub fCutCol, fNxtCol, fCurCol
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutStq, fCutStq, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutStq, fCurStq, fCutStq
        add fCutCol, fCurCol, fCutCol
        sq fCutPos, 0(iOut)
        sq fCutStq, 1(iOut)
        sq fCutCol, 2(iOut)
        sq.x fCurStq, 1(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXlNoCut:
        iaddiu iWalk, iWalk, 3
        iaddi  iLeft, iLeft, -1
        ibgtz  iLeft, lXlLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lXlWrap
    iaddiu iCount,   vi00, 0
    lXlWrap:
    ibltz iSignCur, lXlOut
    lq fCurPos, 957 + 0(vi00)
    lq fCurStq, 957 + 1(vi00)
    lq fCurCol, 957 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurStq, 1(iOut)
    sq fCurCol, 2(iOut)
    isw.x iCount, 1009(vi00)
    lXlOut:

    ilw.x  iLeft,  1009(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1009(vi00)
    iaddi iSignCur, iLeft, -3
    ibltz iSignCur, lXrOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lXrLoop:
        lq fCurPos, 0(iWalk)
        lq fCurStq, 1(iWalk)
        lq fCurCol, 2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtStq, 4(iWalk)
        lq fNxtCol, 5(iWalk)
        mul.xyz fJpC,  fCurPos, fClipScale
        mul.xyz fJpN,  fNxtPos, fClipScale
        addw.x  fDCur, vf00,    fCurPos
        addw.x  fDNxt, vf00,    fNxtPos
        sub.xyz fJnC,  vf00,    fJpC
        sub.xyz fJnN,  vf00,    fJpN
        mul.x   fDCur, fDCur,   fGSScale[w]
        mul.x   fDNxt, fDNxt,   fGSScale[w]
        add.x   fDCur, fDCur,   fJpC[x]
        add.x   fDNxt, fDNxt,   fJpN[x]
        mul.x   fDCur, fDCur,   fGSOffset[w]
        mul.x   fDNxt, fDNxt,   fGSOffset[w]
        mul.x   fClipP,    fDCur,   fDNxt
        mini.x  fClipT,    fDCur,   vf00[w]
        mini.x  fClipP,    fClipP,  vf00[w]
        max.x   fClipT,    fClipT,  fMinusOne[x]
        max.x   fClipP,    fClipP,  fMinusOne[x]
        ftoi4.x fClipT,    fClipT
        ftoi4.x fClipP,    fClipP
        mtir    iSignCur,  fClipT[x]
        mtir    iSignProd, fClipP[x]
        ibltz iSignCur, lXrKept
        sq fCurPos, 0(iOut)
        sq fCurStq, 1(iOut)
        sq fCurCol, 2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXrKept:
        ibgez iSignProd, lXrNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutStq, fNxtStq, fCurStq
        sub fCutCol, fNxtCol, fCurCol
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutStq, fCutStq, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutStq, fCurStq, fCutStq
        add fCutCol, fCurCol, fCutCol
        sq fCutPos, 0(iOut)
        sq fCutStq, 1(iOut)
        sq fCutCol, 2(iOut)
        sq.x fCurStq, 1(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXrNoCut:
        iaddiu iWalk, iWalk, 3
        iaddi  iLeft, iLeft, -1
        ibgtz  iLeft, lXrLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lXrWrap
    iaddiu iCount,   vi00, 0
    lXrWrap:
    ibltz iSignCur, lXrOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurStq, 981 + 1(vi00)
    lq fCurCol, 981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurStq, 1(iOut)
    sq fCurCol, 2(iOut)
    isw.x iCount, 1009(vi00)
    lXrOut:

    ilw.x  iLeft,  1009(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1009(vi00)
    iaddi iSignCur, iLeft, -3
    ibltz iSignCur, lYbOut
    iaddiu iWalk, vi00, 981
    iaddiu iOut,  vi00, 957
    lYbLoop:
        lq fCurPos, 0(iWalk)
        lq fCurStq, 1(iWalk)
        lq fCurCol, 2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtStq, 4(iWalk)
        lq fNxtCol, 5(iWalk)
        mul.xyz fJpC,  fCurPos, fClipScale
        mul.xyz fJpN,  fNxtPos, fClipScale
        addw.x  fDCur, vf00,    fCurPos
        addw.x  fDNxt, vf00,    fNxtPos
        sub.xyz fJnC,  vf00,    fJpC
        sub.xyz fJnN,  vf00,    fJpN
        mul.x   fDCur, fDCur,   fGSScale[w]
        mul.x   fDNxt, fDNxt,   fGSScale[w]
        add.x   fDCur, fDCur,   fJnC[y]
        add.x   fDNxt, fDNxt,   fJnN[y]
        mul.x   fDCur, fDCur,   fGSOffset[w]
        mul.x   fDNxt, fDNxt,   fGSOffset[w]
        mul.x   fClipP,    fDCur,   fDNxt
        mini.x  fClipT,    fDCur,   vf00[w]
        mini.x  fClipP,    fClipP,  vf00[w]
        max.x   fClipT,    fClipT,  fMinusOne[x]
        max.x   fClipP,    fClipP,  fMinusOne[x]
        ftoi4.x fClipT,    fClipT
        ftoi4.x fClipP,    fClipP
        mtir    iSignCur,  fClipT[x]
        mtir    iSignProd, fClipP[x]
        ibltz iSignCur, lYbKept
        sq fCurPos, 0(iOut)
        sq fCurStq, 1(iOut)
        sq fCurCol, 2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYbKept:
        ibgez iSignProd, lYbNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutStq, fNxtStq, fCurStq
        sub fCutCol, fNxtCol, fCurCol
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutStq, fCutStq, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutStq, fCurStq, fCutStq
        add fCutCol, fCurCol, fCutCol
        sq fCutPos, 0(iOut)
        sq fCutStq, 1(iOut)
        sq fCutCol, 2(iOut)
        sq.x fCurStq, 1(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYbNoCut:
        iaddiu iWalk, iWalk, 3
        iaddi  iLeft, iLeft, -1
        ibgtz  iLeft, lYbLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lYbWrap
    iaddiu iCount,   vi00, 0
    lYbWrap:
    ibltz iSignCur, lYbOut
    lq fCurPos, 957 + 0(vi00)
    lq fCurStq, 957 + 1(vi00)
    lq fCurCol, 957 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurStq, 1(iOut)
    sq fCurCol, 2(iOut)
    isw.x iCount, 1009(vi00)
    lYbOut:

    ilw.x  iLeft,  1009(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1009(vi00)
    iaddi iSignCur, iLeft, -3
    ibltz iSignCur, lYtOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lYtLoop:
        lq fCurPos, 0(iWalk)
        lq fCurStq, 1(iWalk)
        lq fCurCol, 2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtStq, 4(iWalk)
        lq fNxtCol, 5(iWalk)
        mul.xyz fJpC,  fCurPos, fClipScale
        mul.xyz fJpN,  fNxtPos, fClipScale
        addw.x  fDCur, vf00,    fCurPos
        addw.x  fDNxt, vf00,    fNxtPos
        sub.xyz fJnC,  vf00,    fJpC
        sub.xyz fJnN,  vf00,    fJpN
        mul.x   fDCur, fDCur,   fGSScale[w]
        mul.x   fDNxt, fDNxt,   fGSScale[w]
        add.x   fDCur, fDCur,   fJpC[y]
        add.x   fDNxt, fDNxt,   fJpN[y]
        mul.x   fDCur, fDCur,   fGSOffset[w]
        mul.x   fDNxt, fDNxt,   fGSOffset[w]
        mul.x   fClipP,    fDCur,   fDNxt
        mini.x  fClipT,    fDCur,   vf00[w]
        mini.x  fClipP,    fClipP,  vf00[w]
        max.x   fClipT,    fClipT,  fMinusOne[x]
        max.x   fClipP,    fClipP,  fMinusOne[x]
        ftoi4.x fClipT,    fClipT
        ftoi4.x fClipP,    fClipP
        mtir    iSignCur,  fClipT[x]
        mtir    iSignProd, fClipP[x]
        ibltz iSignCur, lYtKept
        sq fCurPos, 0(iOut)
        sq fCurStq, 1(iOut)
        sq fCurCol, 2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYtKept:
        ibgez iSignProd, lYtNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutStq, fNxtStq, fCurStq
        sub fCutCol, fNxtCol, fCurCol
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutStq, fCutStq, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutStq, fCurStq, fCutStq
        add fCutCol, fCurCol, fCutCol
        sq fCutPos, 0(iOut)
        sq fCutStq, 1(iOut)
        sq fCutCol, 2(iOut)
        sq.x fCurStq, 1(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYtNoCut:
        iaddiu iWalk, iWalk, 3
        iaddi  iLeft, iLeft, -1
        ibgtz  iLeft, lYtLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lYtWrap
    iaddiu iCount,   vi00, 0
    lYtWrap:
    ibltz iSignCur, lYtOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurStq, 981 + 1(vi00)
    lq fCurCol, 981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurStq, 1(iOut)
    sq fCurCol, 2(iOut)
    isw.x iCount, 1009(vi00)
    lYtOut:

    lFan:
        ilw.x iCount, 1009(vi00)
        iaddi iLeft, iCount, -2
        ibltz iLeft, lClipDone
        ibeq  iLeft, vi00, lClipDone
        lq fPos0, 981 + 0(vi00)
        lq fStq0, 981 + 1(vi00)
        lq fCol0, 981 + 2(vi00)
        iaddiu iFan, vi00, 981 + 3
        lFanLoop:
            lq fPos1, 0(iFan)
            lq fStq1, 1(iFan)
            lq fCol1, 2(iFan)
            lq fPos2, 3(iFan)
            lq fStq2, 4(iFan)
            lq fCol2, 5(iFan)

    mul.xyz   fJudge, fPos0, fClipScale
    clipw.xyz fJudge, fPos0[w]

    mul.xyz   fJudge, fPos1, fClipScale
    clipw.xyz fJudge, fPos1[w]

    mul.xyz   fJudge, fPos2, fClipScale
    clipw.xyz fJudge, fPos2[w]

    div        q,          vf00[w], fPos0[w]
    mul.xyz    fProj,      fPos0,    q
    mulq       fStqScaled, fStq0,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  0(iOutPtr)
    sq.xyz fProj, 2(iOutPtr)
    ibeq iColorMode, vi00, lEmitFan0Packed
    ftoi0 fRGBA, fCol0
    sq    fRGBA, 1(iOutPtr)
    b lEmitFan0Done
    lEmitFan0Packed:
    sq.x   fStq0, 1(iOutPtr)
    sq.y   fQ,   1(iOutPtr)
    isw.z  iRegRGBAQ, 1(iOutPtr)
    lEmitFan0Done:

    div        q,          vf00[w], fPos1[w]
    mul.xyz    fProj,      fPos1,    q
    mulq       fStqScaled, fStq1,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  3(iOutPtr)
    sq.xyz fProj, 5(iOutPtr)
    ibeq iColorMode, vi00, lEmitFan1Packed
    ftoi0 fRGBA, fCol1
    sq    fRGBA, 4(iOutPtr)
    b lEmitFan1Done
    lEmitFan1Packed:
    sq.x   fStq1, 4(iOutPtr)
    sq.y   fQ,   4(iOutPtr)
    isw.z  iRegRGBAQ, 4(iOutPtr)
    lEmitFan1Done:

    div        q,          vf00[w], fPos2[w]
    mul.xyz    fProj,      fPos2,    q
    mulq       fStqScaled, fStq2,    q
    addq.y     fQ,         vf00,    q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj, fGSScale
    ftoi4.xyz fProj, fProj
    mr32 fST, fStqScaled
    addq.z fST, vf00, q
    sq     fST,  6(iOutPtr)
    sq.xyz fProj, 8(iOutPtr)
    ibeq iColorMode, vi00, lEmitFan2Packed
    ftoi0 fRGBA, fCol2
    sq    fRGBA, 7(iOutPtr)
    b lEmitFan2Done
    lEmitFan2Packed:
    sq.x   fStq2, 7(iOutPtr)
    sq.y   fQ,   7(iOutPtr)
    isw.z  iRegRGBAQ, 7(iOutPtr)
    lEmitFan2Done:

    fcand  vi01, 0x3FFFF
    iaddiu iADC, vi01, 0x7FFF
    isw.w  iADC, 2(iOutPtr)
    isw.w  iADC, 5(iOutPtr)
    isw.w  iADC, 8(iOutPtr)

            iaddiu iOutPtr,    iOutPtr,     9
            iaddi  iVertsLeft, iVertsLeft, -3
            iaddiu iFan,  iFan,  3
            iaddi  iLeft, iLeft, -1
            ibgtz  iLeft, lFanLoop
        lClipDone:
    lTriangleAdvance:
        ilw.y  iStride,   1009(vi00)
        iadd   iInPtr,    iInPtr,    iStride
        iaddi  iNumVerts, iNumVerts, -3
        ibgtz  iNumVerts, lTriangleLoop

    iaddiu iNloop, vi00,   45
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lKicked3
    ilw.x  iWin,   1008(vi00)
    ilw.y  iDelta, 1008(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 6(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    1008(vi00)
    isw.y  iDelta,  1008(vi00)
    --barrier
    xgkick iKickAt
    lKicked3:

--exit
--endexit

