.init_vf_all
.init_vi_all
--enter
in_vf mat0(vf01)
in_vf mat1(vf02)
in_vf vec(vf03)
--endenter
mulax.xyzw acc, mat0, vec
madday.xyzw acc, mat1, vec
maddw.xyzw res, mat0, vf00
div q, vf00w, res.w
mulq.xyz res, res, q
--exit
out_vf res(vf04)
--endexit
