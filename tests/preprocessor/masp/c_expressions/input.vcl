; masp's integer expressions follow C's rules too.
mode    .assigna    1 ? 0 : 1 ? 1 : 1
        .aif        \&mode EQ 0
        right_grouping
        .aendi
        .aif        0 && 1 / 0
        .aelse
        short_circuit
        .aendi
steps   .equ        2 ? 3 ? 4 : 5 : 6
        .arepeat    steps - 3
        repeated
        .aendr
        .end
