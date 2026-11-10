; oxide-vcl corpus program doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips
; From https://github.com/glampert/doom3-bfg-ps2 at adc427d: src/ps2/renderer/vu1progs/world_strips.vcl
; Produced: vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j (vclpp 95911cd)
; License: GPL-3.0-or-later, see tests/corpus/licenses/GPL-3.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --

.name world_strips

.init_vi VI02, VI03, VI04, VI05, VI06, VI07, VI08, VI09, VI10, VI11, VI12, VI13, VI14
.init_vf_all
.syntax new
.vu

--enter
--endenter

    fcset 0x000000
    lq fMVP0,      0 + 0(vi00)
    lq fMVP1,      0 + 1(vi00)
    lq fMVP2,      0 + 2(vi00)
    lq fMVP3,      0 + 3(vi00)
    lq fGSScale,   0 + 4(vi00)
    lq fGSOffset,  0 + 5(vi00)
    lq fClipScale, 0 + 6(vi00)

    sub.x fMinusOne, vf00, vf00[w]
    xtop   iBase
    xitop  iVertsToGo
    lq     fPosOffset, 0(iBase)
    lq     fPosScale,  1(iBase)
    iaddiu iVert,      iBase, 2
    iaddiu iKickAt, iBase, 297
    xgkick iKickAt
    mr32 fUVOffset, fPosScale
    mr32 fUVOffset, fUVOffset
    iaddiu iFlagBit, vi00,     0x7FFF
    iaddiu iFlagBit, iFlagBit, 1
    iaddiu iAnyOut,  vi00,     0
    lTransform:
        lq.xyz fPos, 0(iVert)
        lq.xy  fUV,  1(iVert)
        ilw.w  iAdc, 0(iVert)
        --barrier
        itof0.xyz fPos, fPos
        itof0.xy  fUV,  fUV
        mul.xyz   fPos, fPos, fPosScale[x]
        mul.xy    fUV,  fUV,  fPosScale[y]
        add.xyz   fPos, fPos, fPosOffset
        add.xy    fUV,  fUV,  fUVOffset
        mul  acc,   fMVP0, fPos[x]
        madd acc,   fMVP1, fPos[y]
        madd acc,   fMVP2, fPos[z]
        madd fClip, fMVP3, vf00[w]
        mul.xyz   fJudge, fClip, fClipScale
        clipw.xyz fJudge, fClip[w]
        div       q,     vf00[w],   fClip[w]
        mul.xyz   fProj, fClip,     q
        mul.xy    fSTQ,  fUV,       q
        addq.z    fSTQ,  vf00,      q
        mula.xyz  acc,   fGSOffset, vf00[w]
        madd.xyz  fProj, fProj,     fGSScale
        ftoi4.xyz fProj, fProj
        fcand  vi01,     0x00003F
        iaddiu iOutside, vi01,     0x3FFF
        iaddiu iOutBit,  vi00,     0x4000
        iand   iOutside, iOutside, iOutBit
        ior    iAnyOut,  iAnyOut,  iOutside
        iand   iAdc,     iAdc,     iFlagBit
        ior    iAdc,     iAdc,     iOutside
        sq     fClip, 0(iVert)
        sq.xyz fProj, 1(iVert)
        isw.w  iAdc,  1(iVert)
        sq.xyz fSTQ,  3(iVert)
        iaddiu iVert,      iVert,       4
        iaddi  iVertsToGo, iVertsToGo, -1
        ibgtz  iVertsToGo, lTransform
    ibne iAnyOut, vi00, lListPath

    xtop   iOutPtr
    lq     fTag,    304(iOutPtr)
    ilw.x  iOutPtr, 7(vi00)
    sqi    fTag,    (iOutPtr++)
    iaddiu iVertsLeft, vi00, 50

    iaddiu iVert,  iBase, 2
    xitop  iToGo
    lStripVertex:
        ibne iVertsLeft, vi00, lStripRoom

    iaddiu iNloop, vi00,   50
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lStripFull
    ilw.x  iWin,   7(vi00)
    ilw.y  iDelta, 7(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 0(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    7(vi00)
    isw.y  iDelta,  7(vi00)
    --barrier
    xgkick iKickAt
    lStripFull:

    xtop   iOutPtr
    lq     fTag,    304(iOutPtr)
    ilw.x  iOutPtr, 7(vi00)
    sqi    fTag,    (iOutPtr++)
    iaddiu iVertsLeft, vi00, 50

    lq     fStq, -5(iVert)
    lq     fCol, -6(iVert)
    lq     fXyz, -7(iVert)
    sq     fStq, 0(iOutPtr)
    sq     fCol, 1(iOutPtr)
    sq.xyz fXyz, 2(iOutPtr)
    isw.w  iFlagBit, 2(iOutPtr)
    iaddiu iOutPtr,    iOutPtr,     3
    iaddi  iVertsLeft, iVertsLeft, -1

    lq     fStq, -1(iVert)
    lq     fCol, -2(iVert)
    lq     fXyz, -3(iVert)
    sq     fStq, 0(iOutPtr)
    sq     fCol, 1(iOutPtr)
    sq.xyz fXyz, 2(iOutPtr)
    isw.w  iFlagBit, 2(iOutPtr)
    iaddiu iOutPtr,    iOutPtr,     3
    iaddi  iVertsLeft, iVertsLeft, -1

        lStripRoom:
        lq fStq, 3(iVert)
        lq fCol, 2(iVert)
        lq fXyz, 1(iVert)
        sq fStq, 0(iOutPtr)
        sq fCol, 1(iOutPtr)
        sq fXyz, 2(iOutPtr)
        iaddiu iOutPtr,    iOutPtr,     3
        iaddi  iVertsLeft, iVertsLeft, -1
        iaddiu iVert,      iVert,       4
        iaddi  iToGo,      iToGo,      -1
        ibgtz  iToGo,      lStripVertex

    iaddiu iNloop, vi00,   50
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lStripLast
    ilw.x  iWin,   7(vi00)
    ilw.y  iDelta, 7(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 0(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    7(vi00)
    isw.y  iDelta,  7(vi00)
    --barrier
    xgkick iKickAt
    lStripLast:

    b lDone
    lListPath:

    xtop   iOutPtr
    lq     fTag,    305(iOutPtr)
    ilw.x  iOutPtr, 7(vi00)
    sqi    fTag,    (iOutPtr++)
    iaddiu iVertsLeft, vi00, 50

    xitop  iLeft
    iaddiu iNext, iBase, 2
    isw.x  iLeft, 954(vi00)
    isw.y  iNext, 954(vi00)
    isw.z  vi00,  955(vi00)
    --barrier
    lListEntry:
        iaddiu iRoom, vi00,       18
        isub   iRoom, iVertsLeft, iRoom
        ibgez  iRoom, lListRoom

    iaddiu iNloop, vi00,   50
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lListFull
    ilw.x  iWin,   7(vi00)
    ilw.y  iDelta, 7(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 0(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    7(vi00)
    isw.y  iDelta,  7(vi00)
    --barrier
    xgkick iKickAt
    lListFull:

    xtop   iOutPtr
    lq     fTag,    305(iOutPtr)
    ilw.x  iOutPtr, 7(vi00)
    sqi    fTag,    (iOutPtr++)
    iaddiu iVertsLeft, vi00, 50

        lListRoom:
        ilw.y  iVertex, 954(vi00)
        ilw.w  iPrev1,  954(vi00)
        ilw.x  iPrev2,  955(vi00)
        iaddiu iNext,   iVertex, 4
        --barrier
        isw.y  iNext,   954(vi00)
        isw.x  iPrev1,  955(vi00)
        isw.w  iVertex, 954(vi00)
        ilw.w  iF2,   1(iVertex)
        iaddiu iMask, vi00,  0x7FFF
        iaddiu iMask, iMask, 1
        iand   iAdc,  iF2,   iMask
        ibne   iAdc,  vi00,  lListNext
        ilw.w  iF0,   1(iPrev2)
        ilw.w  iF1,   1(iPrev1)
        ior    iF0,   iF0,   iF1
        ior    iF0,   iF0,   iF2
        iaddiu iMask, vi00,  0x4000
        iand   iF0,   iF0,   iMask
        ibne   iF0,   vi00,  lListClip

    lq     fStq, 3(iPrev2)
    lq     fCol, 2(iPrev2)
    lq     fXyz, 1(iPrev2)
    sq     fStq, 0(iOutPtr)
    sq     fCol, 0 + 1(iOutPtr)
    sq.xyz fXyz, 0 + 2(iOutPtr)
    isw.w  vi00, 0 + 2(iOutPtr)

    lq     fStq, 3(iPrev1)
    lq     fCol, 2(iPrev1)
    lq     fXyz, 1(iPrev1)
    sq     fStq, 3(iOutPtr)
    sq     fCol, 3 + 1(iOutPtr)
    sq.xyz fXyz, 3 + 2(iOutPtr)
    isw.w  vi00, 3 + 2(iOutPtr)

    lq     fStq, 3(iVertex)
    lq     fCol, 2(iVertex)
    lq     fXyz, 1(iVertex)
    sq     fStq, 6(iOutPtr)
    sq     fCol, 6 + 1(iOutPtr)
    sq.xyz fXyz, 6 + 2(iOutPtr)
    isw.w  vi00, 6 + 2(iOutPtr)

        iaddiu iOutPtr,    iOutPtr,     9
        iaddi  iVertsLeft, iVertsLeft, -3
        b lListNext
        lListClip:

    lq    fPass, 955(vi00)
    itof0.z fPass, fPass
    lq    fPos0,  0(iPrev2)
    lq    fColI0, 2(iPrev2)
    lq    fUV0,   3(iPrev2)
    add.x  fLm0,  vf00, fUV0[w]
    add.y  fLm0,  vf00, fPos0[w]
    sub.xy fLm0,  fLm0,  fUV0
    mul.xy fLm0,  fLm0,  fPass[z]
    add.xy fUV0,  fUV0,  fLm0
    div    q,    vf00[w], fUV0[z]
    mul.w  fPos0, vf00,    q
    mul.xy fUV0,  fUV0,     fPos0[w]
    itof0  fCol0, fColI0
    lq    fPos1,  0(iPrev1)
    lq    fColI1, 2(iPrev1)
    lq    fUV1,   3(iPrev1)
    add.x  fLm1,  vf00, fUV1[w]
    add.y  fLm1,  vf00, fPos1[w]
    sub.xy fLm1,  fLm1,  fUV1
    mul.xy fLm1,  fLm1,  fPass[z]
    add.xy fUV1,  fUV1,  fLm1
    div    q,    vf00[w], fUV1[z]
    mul.w  fPos1, vf00,    q
    mul.xy fUV1,  fUV1,     fPos1[w]
    itof0  fCol1, fColI1
    lq    fPos2,  0(iVertex)
    lq    fColI2, 2(iVertex)
    lq    fUV2,   3(iVertex)
    add.x  fLm2,  vf00, fUV2[w]
    add.y  fLm2,  vf00, fPos2[w]
    sub.xy fLm2,  fLm2,  fUV2
    mul.xy fLm2,  fLm2,  fPass[z]
    add.xy fUV2,  fUV2,  fLm2
    div    q,    vf00[w], fUV2[z]
    mul.w  fPos2, vf00,    q
    mul.xy fUV2,  fUV2,     fPos2[w]
    itof0  fCol2, fColI2
    sq fPos0, 957 + 0(vi00)
    sq fCol0, 957 + 1(vi00)
    sq fUV0,  957 + 2(vi00)
    sq fPos1, 957 + 3(vi00)
    sq fCol1, 957 + 4(vi00)
    sq fUV1,  957 + 5(vi00)
    sq fPos2, 957 + 6(vi00)
    sq fCol2, 957 + 7(vi00)
    sq fUV2,  957 + 8(vi00)
    sq fPos0, 957 + 9(vi00)
    sq fCol0, 957 + 10(vi00)
    sq fUV0,  957 + 11(vi00)
    iaddiu iCount, vi00, 3
    isw.x  iCount, 1008(vi00)

    ilw.x  iCorners, 1008(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1008(vi00)
    iaddi iSignCur, iCorners, -3
    ibltz iSignCur, lNearOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lNearLoop:
        lq fCurPos, 0(iWalk)
        lq fCurCol, 1(iWalk)
        lq fCurUV,  2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtCol, 4(iWalk)
        lq fNxtUV,  5(iWalk)
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
        sq fCurCol, 1(iOut)
        sq fCurUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lNearKept:
        ibgez iSignProd, lNearNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutCol, fNxtCol, fCurCol
        sub fCutUV,  fNxtUV,  fCurUV
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        mul fCutUV,  fCutUV,  fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutCol, fCurCol, fCutCol
        add fCutUV,  fCurUV,  fCutUV
        sq fCutPos, 0(iOut)
        sq fCutCol, 1(iOut)
        sq fCutUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lNearNoCut:
        iaddiu iWalk,    iWalk,    3
        iaddi  iCorners, iCorners, -1
        ibgtz  iCorners, lNearLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lNearWrap
    iaddiu iCount,   vi00, 0
    lNearWrap:
    ibltz iSignCur, lNearOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurCol, 981 + 1(vi00)
    lq fCurUV,  981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurCol, 1(iOut)
    sq fCurUV,  2(iOut)
    isw.x iCount, 1008(vi00)
    lNearOut:

    ilw.x  iCorners, 1008(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1008(vi00)
    iaddi iSignCur, iCorners, -3
    ibltz iSignCur, lXlOut
    iaddiu iWalk, vi00, 981
    iaddiu iOut,  vi00, 957
    lXlLoop:
        lq fCurPos, 0(iWalk)
        lq fCurCol, 1(iWalk)
        lq fCurUV,  2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtCol, 4(iWalk)
        lq fNxtUV,  5(iWalk)
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
        sq fCurCol, 1(iOut)
        sq fCurUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXlKept:
        ibgez iSignProd, lXlNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutCol, fNxtCol, fCurCol
        sub fCutUV,  fNxtUV,  fCurUV
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        mul fCutUV,  fCutUV,  fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutCol, fCurCol, fCutCol
        add fCutUV,  fCurUV,  fCutUV
        sq fCutPos, 0(iOut)
        sq fCutCol, 1(iOut)
        sq fCutUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXlNoCut:
        iaddiu iWalk,    iWalk,    3
        iaddi  iCorners, iCorners, -1
        ibgtz  iCorners, lXlLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lXlWrap
    iaddiu iCount,   vi00, 0
    lXlWrap:
    ibltz iSignCur, lXlOut
    lq fCurPos, 957 + 0(vi00)
    lq fCurCol, 957 + 1(vi00)
    lq fCurUV,  957 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurCol, 1(iOut)
    sq fCurUV,  2(iOut)
    isw.x iCount, 1008(vi00)
    lXlOut:

    ilw.x  iCorners, 1008(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1008(vi00)
    iaddi iSignCur, iCorners, -3
    ibltz iSignCur, lXrOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lXrLoop:
        lq fCurPos, 0(iWalk)
        lq fCurCol, 1(iWalk)
        lq fCurUV,  2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtCol, 4(iWalk)
        lq fNxtUV,  5(iWalk)
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
        sq fCurCol, 1(iOut)
        sq fCurUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXrKept:
        ibgez iSignProd, lXrNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutCol, fNxtCol, fCurCol
        sub fCutUV,  fNxtUV,  fCurUV
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        mul fCutUV,  fCutUV,  fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutCol, fCurCol, fCutCol
        add fCutUV,  fCurUV,  fCutUV
        sq fCutPos, 0(iOut)
        sq fCutCol, 1(iOut)
        sq fCutUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lXrNoCut:
        iaddiu iWalk,    iWalk,    3
        iaddi  iCorners, iCorners, -1
        ibgtz  iCorners, lXrLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lXrWrap
    iaddiu iCount,   vi00, 0
    lXrWrap:
    ibltz iSignCur, lXrOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurCol, 981 + 1(vi00)
    lq fCurUV,  981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurCol, 1(iOut)
    sq fCurUV,  2(iOut)
    isw.x iCount, 1008(vi00)
    lXrOut:

    ilw.x  iCorners, 1008(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1008(vi00)
    iaddi iSignCur, iCorners, -3
    ibltz iSignCur, lYbOut
    iaddiu iWalk, vi00, 981
    iaddiu iOut,  vi00, 957
    lYbLoop:
        lq fCurPos, 0(iWalk)
        lq fCurCol, 1(iWalk)
        lq fCurUV,  2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtCol, 4(iWalk)
        lq fNxtUV,  5(iWalk)
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
        sq fCurCol, 1(iOut)
        sq fCurUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYbKept:
        ibgez iSignProd, lYbNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutCol, fNxtCol, fCurCol
        sub fCutUV,  fNxtUV,  fCurUV
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        mul fCutUV,  fCutUV,  fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutCol, fCurCol, fCutCol
        add fCutUV,  fCurUV,  fCutUV
        sq fCutPos, 0(iOut)
        sq fCutCol, 1(iOut)
        sq fCutUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYbNoCut:
        iaddiu iWalk,    iWalk,    3
        iaddi  iCorners, iCorners, -1
        ibgtz  iCorners, lYbLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lYbWrap
    iaddiu iCount,   vi00, 0
    lYbWrap:
    ibltz iSignCur, lYbOut
    lq fCurPos, 957 + 0(vi00)
    lq fCurCol, 957 + 1(vi00)
    lq fCurUV,  957 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurCol, 1(iOut)
    sq fCurUV,  2(iOut)
    isw.x iCount, 1008(vi00)
    lYbOut:

    ilw.x  iCorners, 1008(vi00)
    iaddiu iCount, vi00, 0
    isw.x  iCount, 1008(vi00)
    iaddi iSignCur, iCorners, -3
    ibltz iSignCur, lYtOut
    iaddiu iWalk, vi00, 957
    iaddiu iOut,  vi00, 981
    lYtLoop:
        lq fCurPos, 0(iWalk)
        lq fCurCol, 1(iWalk)
        lq fCurUV,  2(iWalk)
        lq fNxtPos, 3(iWalk)
        lq fNxtCol, 4(iWalk)
        lq fNxtUV,  5(iWalk)
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
        sq fCurCol, 1(iOut)
        sq fCurUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYtKept:
        ibgez iSignProd, lYtNoCut
        sub.x  fClipT, fDCur,    fDNxt
        div    q,      fDCur[x], fClipT[x]
        addq.x fClipQ, vf00,     q
        sub fCutPos, fNxtPos, fCurPos
        sub fCutCol, fNxtCol, fCurCol
        sub fCutUV,  fNxtUV,  fCurUV
        mul fCutPos, fCutPos, fClipQ[x]
        mul fCutCol, fCutCol, fClipQ[x]
        mul fCutUV,  fCutUV,  fClipQ[x]
        add fCutPos, fCurPos, fCutPos
        add fCutCol, fCurCol, fCutCol
        add fCutUV,  fCurUV,  fCutUV
        sq fCutPos, 0(iOut)
        sq fCutCol, 1(iOut)
        sq fCutUV,  2(iOut)
        iaddiu iOut,   iOut,   3
        iaddiu iCount, iCount, 1
        lYtNoCut:
        iaddiu iWalk,    iWalk,    3
        iaddi  iCorners, iCorners, -1
        ibgtz  iCorners, lYtLoop
    iaddi  iSignCur, iCount, -3
    ibgez  iSignCur, lYtWrap
    iaddiu iCount,   vi00, 0
    lYtWrap:
    ibltz iSignCur, lYtOut
    lq fCurPos, 981 + 0(vi00)
    lq fCurCol, 981 + 1(vi00)
    lq fCurUV,  981 + 2(vi00)
    sq fCurPos, 0(iOut)
    sq fCurCol, 1(iOut)
    sq fCurUV,  2(iOut)
    isw.x iCount, 1008(vi00)
    lYtOut:

        ilw.x iCount,   1008(vi00)
        iaddi iFanLeft, iCount, -2
        ibltz iFanLeft, lListNext
        ibeq  iFanLeft, vi00, lListNext
        lq fPos0, 981 + 0(vi00)
        lq fCol0, 981 + 1(vi00)
        lq fUV0,  981 + 2(vi00)
        iaddiu iFan, vi00, 981 + 3
        lFanLoop:
            lq fPos1, 0(iFan)
            lq fCol1, 1(iFan)
            lq fUV1,  2(iFan)
            lq fPos2, 3(iFan)
            lq fCol2, 4(iFan)
            lq fUV2,  5(iFan)

    mul.xyz   fJudge, fPos0, fClipScale
    clipw.xyz fJudge, fPos0[w]

    mul.xyz   fJudge, fPos1, fClipScale
    clipw.xyz fJudge, fPos1[w]

    mul.xyz   fJudge, fPos2, fClipScale
    clipw.xyz fJudge, fPos2[w]

    div       q,     vf00[w],   fPos0[w]
    mul.xyz   fProj, fPos0,      q
    mul.xy    fSTQ,  fUV0,       q
    addq.z    fSTQ,  vf00,      q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj,     fGSScale
    ftoi4.xyz fProj, fProj
    ftoi0     fRGBA, fCol0
    sq.xyz fSTQ,  0(iOutPtr)
    sq     fRGBA, 1(iOutPtr)
    sq.xyz fProj, 2(iOutPtr)

    div       q,     vf00[w],   fPos1[w]
    mul.xyz   fProj, fPos1,      q
    mul.xy    fSTQ,  fUV1,       q
    addq.z    fSTQ,  vf00,      q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj,     fGSScale
    ftoi4.xyz fProj, fProj
    ftoi0     fRGBA, fCol1
    sq.xyz fSTQ,  3(iOutPtr)
    sq     fRGBA, 4(iOutPtr)
    sq.xyz fProj, 5(iOutPtr)

    div       q,     vf00[w],   fPos2[w]
    mul.xyz   fProj, fPos2,      q
    mul.xy    fSTQ,  fUV2,       q
    addq.z    fSTQ,  vf00,      q
    mula.xyz  acc,   fGSOffset, vf00[w]
    madd.xyz  fProj, fProj,     fGSScale
    ftoi4.xyz fProj, fProj
    ftoi0     fRGBA, fCol2
    sq.xyz fSTQ,  6(iOutPtr)
    sq     fRGBA, 7(iOutPtr)
    sq.xyz fProj, 8(iOutPtr)

    fcand  vi01, 0x3FFFF
    iaddiu iADC, vi01, 0x7FFF
    isw.w  iADC, 2(iOutPtr)
    isw.w  iADC, 5(iOutPtr)
    isw.w  iADC, 8(iOutPtr)

            iaddiu iOutPtr,    iOutPtr,     9
            iaddi  iVertsLeft, iVertsLeft, -3
            iaddiu iFan,     iFan,     3
            iaddi  iFanLeft, iFanLeft, -1
            ibgtz  iFanLeft, lFanLoop
        lListNext:
        ilw.x iLeft, 954(vi00)
        iaddi iLeft, iLeft, -1
        isw.x iLeft, 954(vi00)
        ibgtz iLeft, lListEntry

    iaddiu iNloop, vi00,   50
    isub   iNloop, iNloop, iVertsLeft
    ibeq   iNloop, vi00, lListLast
    ilw.x  iWin,   7(vi00)
    ilw.y  iDelta, 7(vi00)
    iaddiu iNloop, iNloop, 0x7FFF
    iaddiu iNloop, iNloop, 1
    isw.x  iNloop, 0(iWin)
    --barrier
    iaddiu iKickAt, iWin, 0
    iadd   iWin,    iWin, iDelta
    isub   iDelta,  vi00, iDelta
    isw.x  iWin,    7(vi00)
    isw.y  iDelta,  7(vi00)
    --barrier
    xgkick iKickAt
    lListLast:

    iaddiu iKickAt, vi00, 8
    xgkick iKickAt
    lDone:

--exit
--endexit

