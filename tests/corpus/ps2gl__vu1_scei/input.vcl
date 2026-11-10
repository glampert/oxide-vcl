; oxide-vcl corpus program ps2gl__vu1_scei
; From https://github.com/glampert/ps2gl at 794bedc: vu1/scei.vcl
; Produced: vclpp -m -x -f -I vu1 -I include
; License: LGPL-2.1, see tests/corpus/licenses/LGPL-2.1.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
	.init_vf_all
	.init_vi_all
	.name		vsmSCEI
	--enter
	--endenter

	lq			vert_xform_0, 62(vi00)
	lq			vert_xform_1, 63(vi00)
	lq			vert_xform_2, 64(vi00)
	lq			vert_xform_3, 65(vi00)

	lq.xyz		constant_color, 58(vi00)

	move.xyz		light_dirs_0, vf00
	move.xyz		light_dirs_1, vf00
	move.xyz		light_dirs_2, vf00
	move.xyz		light_colors_0, vf00
	move.xyz		light_colors_1, vf00
	move.xyz		light_colors_2, vf00

	ilw.x		num_dir_lights, (0)(vi00)
	iaddiu		light_ptr_ptr, vi00, 1
	ilw.x		light_ptr, 0(light_ptr_ptr)

	lq.xyz		material_amb, 59(vi00)

	ibeq			num_dir_lights, vi00, finish_init_lid

	lq.xyz		material_diff, 60(vi00)

	lq.xyz		obj_to_world_transpose_0, 67(vi00)
	lq.xyz		obj_to_world_transpose_1, 68(vi00)
	lq.xyz		obj_to_world_transpose_2, 69(vi00)

	lq.xyz		light_dirs_0, 3(light_ptr)

	mulax.xyz		acc, obj_to_world_transpose_0, light_dirs_0
	madday.xyz	acc, obj_to_world_transpose_1, light_dirs_0
	maddz.xyz		light_dirs_0, obj_to_world_transpose_2, light_dirs_0

	lq.xyz		light_colors_0, 1(light_ptr)

	mul.xyz		light_colors_0, light_colors_0, material_diff

	lq.xyz		light_amb, 0(light_ptr)

	mul.xyz		local_amb, material_amb, light_amb
	add.xyz		constant_color, constant_color, local_amb
	isubiu		num_dir_lights, num_dir_lights, 1
	ibeq			num_dir_lights, vi00, finish_init_lid

	iaddiu		light_ptr_ptr, light_ptr_ptr, 1
	ilw.x		light_ptr, 0(light_ptr_ptr)

	lq.xyz		light_dirs_1, 3(light_ptr)

	mulax.xyz		acc, obj_to_world_transpose_0, light_dirs_1
	madday.xyz	acc, obj_to_world_transpose_1, light_dirs_1
	maddz.xyz		light_dirs_1, obj_to_world_transpose_2, light_dirs_1

	lq.xyz		light_colors_1, 1(light_ptr)

	mul.xyz		light_colors_1, light_colors_1, material_diff

	lq.xyz		light_amb, 0(light_ptr)

	mul.xyz		local_amb, material_amb, light_amb
	add.xyz		constant_color, constant_color, local_amb
	isubiu		num_dir_lights, num_dir_lights, 1
	ibeq			num_dir_lights, vi00, finish_init_lid

	iaddiu		light_ptr_ptr, light_ptr_ptr, 1
	ilw.x		light_ptr, 0(light_ptr_ptr)

	lq.xyz		light_dirs_2, 3(light_ptr)

	mulax.xyz		acc, obj_to_world_transpose_0, light_dirs_2
	madday.xyz	acc, obj_to_world_transpose_1, light_dirs_2
	maddz.xyz		light_dirs_2, obj_to_world_transpose_2, light_dirs_2

	lq.xyz		light_colors_2, 1(light_ptr)

	mul.xyz		light_colors_2, light_colors_2, material_diff

	lq.xyz		light_amb, 0(light_ptr)

	mul.xyz		local_amb, material_amb, light_amb
	add.xyz		constant_color, constant_color, local_amb
finish_init_lid:

	lq.xyz		global_amb, 57(vi00)

	mul.xyz		local_amb, material_amb, global_amb
	add.xyz		constant_color, constant_color, local_amb

	addy.x		temp_121, vf00, light_dirs_0
	addx.y		light_dirs_0, vf00, light_dirs_1
	add.x		light_dirs_1, vf00, temp_121
	addz.x		temp_221, vf00, light_dirs_0
	addx.z		light_dirs_0, vf00, light_dirs_2
	add.x		light_dirs_2, vf00, temp_221
	mr32.x		temp121, light_dirs_2
	mr32.w		temp121, temp121
	mr32.y		light_dirs_2, light_dirs_1
	mr32.z		light_dirs_1, temp121

	lq.w		material_diff, 60(vi00)

	loi			128.0
	muli.w		color, material_diff, i
	loi			255.0
	minii.w		color, color, i
	loi			12582912.0
	addi.xyz		constant_color, constant_color, i
	addi.w		color, color, i

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
	iaddiu		next_output, buffer_top, 237
	ilw.x		num_verts, 0(buffer_top)
	iadd			last_input, next_input, num_verts
	iadd			last_input, last_input, num_verts
	iadd			last_input, last_input, num_verts

	lq			gif_tag28, 75(vi00)
	mtir			eop28, gif_tag28x
	ior			eop28, eop28, num_verts
	mfir.x		gif_tag28, eop28
	sq			gif_tag28, -1(next_output)

	iaddiu		firstVert, buffer_top, 5
	iaddiu		adcsPtr, buffer_top, 1
	iaddiu		lastAdcsPtr, adcsPtr, 4
	iaddiu		offsetMask, vi00, 0x3ff
	iaddiu		second_adc_mask, vi00, 0x800
	iaddiu		stop_bit_mask, vi00, 0x400
	iaddiu		first_adc, vi00, 0x20
	adcLoop_lid:
	lq			strip_boundaries, 0(adcsPtr)
	ftoi0		strip_boundaries, strip_boundaries
	mtir			offset, strip_boundariesx
	iand			stop_bit, offset, stop_bit_mask
	ibeq			stop_bit, stop_bit_mask, adcLoop_done_lid
	iand			second_adc, offset, second_adc_mask
	iand			offset, offset, offsetMask
	iadd			offset, offset, firstVert
	isw.w		first_adc, 0(offset)
	isw.w		second_adc, 3(offset)
	mtir			offset, strip_boundariesy
	iand			stop_bit, offset, stop_bit_mask
	ibeq			stop_bit, stop_bit_mask, adcLoop_done_lid
	iand			second_adc, offset, second_adc_mask
	iand			offset, offset, offsetMask
	iadd			offset, offset, firstVert
	isw.w		first_adc, 0(offset)
	isw.w		second_adc, 3(offset)
	mtir			offset, strip_boundariesz
	iand			stop_bit, offset, stop_bit_mask
	ibeq			stop_bit, stop_bit_mask, adcLoop_done_lid
	iand			second_adc, offset, second_adc_mask
	iand			offset, offset, offsetMask
	iadd			offset, offset, firstVert
	isw.w		first_adc, 0(offset)
	isw.w		second_adc, 3(offset)
	mtir			offset, strip_boundariesw
	iand			stop_bit, offset, stop_bit_mask
	ibeq			stop_bit, stop_bit_mask, adcLoop_done_lid
	iand			second_adc, offset, second_adc_mask
	iand			offset, offset, offsetMask
	iadd			offset, offset, firstVert
	isw.w		first_adc, 0(offset)
	isw.w		second_adc, 3(offset)
	iaddiu		adcsPtr, adcsPtr, 1
	ibne			adcsPtr, lastAdcsPtr, adcLoop_lid
	adcLoop_done_lid:

xform_loop_lid:
	--LoopCS		1,2

	lq.xyz		vert, 0(next_input)

	mulax		acc, vert_xform_0, vert
	madday		acc, vert_xform_1, vert
	maddaz		acc, vert_xform_2, vert
	maddw		xformed_vert, vert_xform_3, vf00
	div			q, vf00w, xformed_vertw
	mulq.xyz		xformed_vert, xformed_vert, q

	add.xyz		gs_vert, xformed_vert, gs_offsets
	ftoi4.xyz		gs_vert, gs_vert

	mul.xyz		clip_vert38, xformed_vert, clip_scales
	clipw.xyz		clip_vert38, clip_scalesw

	fcand		vi01, 0x003ffff

	ilw.w		strip_adc, 0(next_input)

	ior			new_adc40, vi01, strip_adc
	iaddiu		new_adc40, new_adc40, 0x7fff
	mfir.w		gs_vert, new_adc40

	sq			gs_vert, 2(next_output)

	lq.xyz		normal, 1(next_input)

	mulax.xyz		acc, light_dirs_0, normal
	madday.xyz	acc, light_dirs_1, normal
	maddz.xyz		cosines, light_dirs_2, normal

	max.xyz		cosines, cosines, vf00

	mulax.xyz		acc, light_colors_0, cosines
	madday.xyz	acc, light_colors_1, cosines
	maddz.xyz		color, light_colors_2, cosines

	add.xyz		color, color, constant_color
	miniw.xyz		color, color, gs_offsetsw

	sq			color, 1(next_output)

	lq.xyz		tex_stq, 2(next_input)

	mulq.xyz		tex_stq, tex_stq, q

	sq.xyz		tex_stq, 0(next_output)

	iaddiu		next_input, next_input, 3
	iaddiu		next_output, next_output, 3

	ibne			next_input, last_input, xform_loop_lid

done_lid:

	iaddiu		packet_start53, buffer_top, 236
	xgkick		packet_start53

	--cont
	b	main_loop_lid
