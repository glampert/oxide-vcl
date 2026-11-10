; oxide-vcl corpus program ps2gl__examples_tricked_out_billboard_renderer
; From https://github.com/glampert/ps2gl at 794bedc: examples/tricked_out/billboard_renderer.vcl
; Produced: vclpp -m -x -f -I examples/tricked_out -I vu1 -I include
; License: LGPL-2.1, see tests/corpus/licenses/LGPL-2.1.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
	.init_vf_all
	.init_vi_all
	.name		vsmBillboards
	--enter
	--endenter

	lq			vert_xform_0, 62(vi00)
	lq			vert_xform_1, 63(vi00)
	lq			vert_xform_2, 64(vi00)
	lq			vert_xform_3, 65(vi00)

	lq.xyz		obj_to_world_transpose_0, 67(vi00)
	lq.xyz		obj_to_world_transpose_1, 68(vi00)
	lq.xyz		obj_to_world_transpose_2, 69(vi00)

	loi			2047.5
	addi.xy		gs_offsets, vf00, i
	lq.w			temp, 57(vi00)
	mr32.z		gs_offsets, temp
	loi			12583167.0
	maxi.w		gs_offsets, vf00, i
	fcset		0x000

	lq.xyz		clip_scales, 76(vi00)
	loi			2048.0
	maxi.w		clip_scales, vf00, i
	ilw.w		do_clipping, 76(vi00)

	--cont
main_loop_lid:

	xtop			buffer_top
	iaddiu		next_input, buffer_top, 5
	ilw.x		num_verts, 0(buffer_top)
	iadd			last_input, next_input, num_verts

	iaddiu		next_output, buffer_top, 52
	lq			giftag, 75(vi00)
	mtir			eop, giftagx
	ior			eop, eop, num_verts
	mfir.x		giftag, eop
	sqi			giftag, (next_output++)
	maxw.xyz			lr_tex_coord, vf00, vf00
	move.xy		ul_tex_coord, vf00
	maxw.z		ul_tex_coord, vf00, vf00
	minix.w		ul_tex_coord, vf00, vf00
	minix.w		lr_tex_coord, vf00, vf00
xform_loop_lid:
	--LoopCS		1,1
	lq			center, 0(next_input)
	sub.xyz		up_left, obj_to_world_transpose_1, obj_to_world_transpose_0
	mulw.xyz		up_left, up_left, center
	add.xyz		up_left, up_left, center
	sub.xyz		down_right, obj_to_world_transpose_0, obj_to_world_transpose_1
	mulw.xyz		down_right, down_right, center
	add.xyz		down_right, down_right, center

	mulax		acc, vert_xform_0, up_left
	madday		acc, vert_xform_1, up_left
	maddaz		acc, vert_xform_2, up_left
	maddw		xformed_vert, vert_xform_3, vf00
	div			q, vf00w, xformed_vertw
	mulq.xyz		xformed_vert, xformed_vert, q

	add.xyz		ul_gs_vert, xformed_vert, gs_offsets
	ftoi4.xyz		ul_gs_vert, ul_gs_vert

	mul.xyz		clip_vert10, xformed_vert, clip_scales
	clipw.xyz		clip_vert10, clip_scalesw

	mulq.xyz		ul_tex_asdf, ul_tex_coord, q

	mulax		acc, vert_xform_0, down_right
	madday		acc, vert_xform_1, down_right
	maddaz		acc, vert_xform_2, down_right
	maddw		xformed_vert, vert_xform_3, vf00
	div			q, vf00w, xformed_vertw
	mulq.xyz		xformed_vert, xformed_vert, q

	add.xyz		lr_gs_vert, xformed_vert, gs_offsets
	ftoi4.xyz		lr_gs_vert, lr_gs_vert

	mul.xyz		clip_vert15, xformed_vert, clip_scales
	clipw.xyz		clip_vert15, clip_scalesw

	mulq.xyz		lr_tex_asdf, lr_tex_coord, q

	fcand		vi01, 0x0000fff
	iaddiu		adc, vi01, 0x7fff
	mfir.w		lr_gs_vert, adc
	mfir.w		ul_gs_vert, adc
	sqi			ul_tex_coord, (next_output++)
	sqi			ul_gs_vert, (next_output++)
	sqi			lr_tex_coord, (next_output++)
	sqi			lr_gs_vert, (next_output++)

	iaddiu		next_input, next_input, 1

	ibne			next_input, last_input, xform_loop_lid

done_lid:

	iaddiu		packet_start19, buffer_top, 52
	xgkick		packet_start19

	--cont
	b	main_loop_lid
