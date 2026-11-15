; #if evaluates ?: and the logical operators as C does: ?: groups to the
; right, either branch can hold another, and an operand C does not evaluate
; is only checked.
#define MODE 2
#vuprog
#if MODE == 1 ? 0 : MODE == 2 ? 1 : 0
    right_grouping
#endif
#if 1 ? 2 : (3 ? 4 : 5)
    nested_else
#endif
#if 2 ? 0 ? 4 : 5 : 6
    nested_then
#endif
#if 0 && 1 / 0
#elif 1 || 1 / 0
    short_circuit
#endif
#if defined(MISSING) && MISSING / 0
#else
    guarded_division
#endif
#if MODE ? MODE / 2 : 1 / 0
    untaken_branch
#endif
#endvuprog
