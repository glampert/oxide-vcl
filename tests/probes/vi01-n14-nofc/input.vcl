.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
in_hw_clip CLIP
--endenter
    lq vPos, 0(vi00)
    ilw.x iVar1, 1(vi00)
    ilw.x iVar2, 2(vi00)
    ilw.x iVar3, 3(vi00)
    ilw.x iVar4, 4(vi00)
    ilw.x iVar5, 5(vi00)
    ilw.x iVar6, 6(vi00)
    ilw.x iVar7, 7(vi00)
    ilw.x iVar8, 8(vi00)
    ilw.x iVar9, 9(vi00)
    ilw.x iVar10, 10(vi00)
    ilw.x iVar11, 11(vi00)
    ilw.x iVar12, 12(vi00)
    ilw.x iVar13, 13(vi00)
    ilw.x iVar14, 14(vi00)
    clipw.xyz vPos, vPos[w]
    isw.x iVar1, 21(vi00)
    isw.x iVar2, 22(vi00)
    isw.x iVar3, 23(vi00)
    isw.x iVar4, 24(vi00)
    isw.x iVar5, 25(vi00)
    isw.x iVar6, 26(vi00)
    isw.x iVar7, 27(vi00)
    isw.x iVar8, 28(vi00)
    isw.x iVar9, 29(vi00)
    isw.x iVar10, 30(vi00)
    isw.x iVar11, 31(vi00)
    isw.x iVar12, 32(vi00)
    isw.x iVar13, 33(vi00)
    isw.x iVar14, 34(vi00)
--exit
--endexit
