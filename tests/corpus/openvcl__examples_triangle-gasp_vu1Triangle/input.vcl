; oxide-vcl corpus program openvcl__examples_triangle-gasp_vu1Triangle
; From https://github.com/glampert/openvcl at 6f8607b: examples/triangle-gasp/vu1Triangle.vcl
; Produced: preprocessed by masp (vcl -g --gasp); the masp source is source.vcl
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
	/*
	* vu1Triangle.vcl
	*
	* Copyright (C) 2004 Jesper Svennevid, Daniel Collin
	*
	* Licensed under the AFL v2.0. See the file LICENSE included with this
	* distribution for licensing terms.
	*
	*/




	///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////


	///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

	.syntax new
	.name vu1Triangle             
	.vu
	.init_vf_all
	.init_vi_all
	--enter
	--endenter

	iaddiu          vertexData, vi00, 6
	iaddiu          destAddress, vi00, 20
	iaddiu          kickAddress, vi00, 20

	lq              worldToScreen[0], 0+0(vi00)
	lq              worldToScreen[1], 0+1(vi00)
	lq              worldToScreen[2], 0+2(vi00)
	lq              worldToScreen[3], 0+3(vi00)

	lq              gifTag, 4(vi00)
	sqi             gifTag, (destAddress++)
	lq              color, 5(vi00)
	sqi             color, (destAddress++)

	iaddiu          vertexCounter, vi00, 3
vertexLoop:
	lqi             vertex, (vertexData++)

	mul            acc,           worldToScreen[0], vertex[x]
	madd           acc,           worldToScreen[1], vertex[y]
	madd           acc,           worldToScreen[2], vertex[z]
	madd           vertexOut, worldToScreen[3], vertex[w]

	div             q, vf00[w], vertexOut[w]
	mul.xyz         vertexOut, vertexOut, q
	ftoi4           vertexOut, vertexOut

	sqi             vertexOut, (destAddress++)

	iaddi           vertexCounter, vertexCounter, -1
	ibne            vertexCounter, vi00, vertexLoop
	xgkick          kickAddress

	--exit
	--endexit
