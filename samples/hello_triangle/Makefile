# hello_triangle: needs the ps2dev toolchain ($PS2SDK, dvp-as on PATH) and cargo.

EE_BIN      = hello_triangle.elf
EE_OBJS     = main.o hello_triangle.o
EE_LIBS     = -ldraw -lgraph -lpacket2 -ldma
EE_CXXFLAGS = -std=c++17

# oxide-vusl, built from this repository.
OXIDE_VCL  = ../..
OXIDE_VUSL = $(OXIDE_VCL)/target/release/oxide-vusl

all: $(EE_BIN)

$(OXIDE_VUSL): $(shell find $(OXIDE_VCL)/crates -name '*.rs') $(OXIDE_VCL)/Cargo.toml
	cargo build --release --bin oxide-vusl --manifest-path $(OXIDE_VCL)/Cargo.toml

# VUSL to a .vsm listing and the C++ header of its memory, then dvp-as.
%.vsm %.vusl.h: %.vusl $(OXIDE_VUSL)
	$(OXIDE_VUSL) --warnings-as-errors --header=$*.vusl.h -o$*.vsm $<

%.o: %.vsm
	dvp-as $< -o $@

main.o: hello_triangle.vusl.h

.SECONDARY: hello_triangle.vsm

clean:
	rm -f $(EE_BIN) $(EE_OBJS) *.vsm *.vusl.h

include $(PS2SDK)/samples/Makefile.pref
include $(PS2SDK)/samples/Makefile.eeglobal_cpp
