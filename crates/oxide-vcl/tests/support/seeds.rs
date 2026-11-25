//! The seeded VU states the listing checks and the benchmarks run from:
//! random states, then one per trip count.

use crate::vu_model::State;

/// Random states, then one state per trip count in [`COUNTS`].
pub const RANDOM_SEEDS: u64 = 4;
/// What counts and pointers read with ILW, and the incoming VI values,
/// are in the count seeds: trip counts 1 to 6 of a loop that counts down
/// by one, and 1 to 4 of one that counts down by three. Pipelined loops
/// exit early from their prologues at small counts.
pub const COUNTS: [u16; 8] = [1, 2, 3, 4, 5, 6, 9, 12];
pub const SEEDS: u64 = RANDOM_SEEDS + COUNTS.len() as u64;
pub const MAX_STEPS: usize = 200_000;

/// A random start: VF values of mixed sign and magnitude; data memory
/// whose low 16 bits are 12, 24, 36 or 48, so that counts and pointers read
/// with ILW stay short and loops stepping by 1, 2, 3, 4 or 6 to a bound
/// reach it; small VI, TOP and ITOP values. A count seed sets those low
/// bits and VI01 to VI15 to its count instead.
pub fn seeded_state(seed: u64) -> State {
    let count = seed
        .checked_sub(RANDOM_SEEDS)
        .map(|index| COUNTS[index as usize]);
    state(seed, count)
}

/// A count seed's state for any `count`, from its own random values: the
/// trip-count sweeps' (docs/TESTING.md, "Benchmarks").
pub fn count_state(count: u16) -> State {
    state(1000 + u64::from(count), Some(count))
}

fn state(seed: u64, count: Option<u16>) -> State {
    let mut random = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let float = |next: &mut dyn FnMut() -> u64| {
        let value = next();
        let sign = (value & 1) as u32;
        let exponent = 123 + (value >> 1) % 9; // 2^-4 to 2^4
        let mantissa = (value >> 8) as u32 & 0x7f_0000;
        sign << 31 | (exponent as u32) << 23 | mantissa
    };
    let mut state = State::default();
    for register in &mut state.vf[1..] {
        *register = std::array::from_fn(|_| float(&mut next));
    }
    for qword in &mut state.memory {
        *qword = std::array::from_fn(|_| {
            let low = count.map_or((next() % 4 + 1) as u32 * 12, u32::from);
            float(&mut next) & !0xffff | low
        });
    }
    for register in &mut state.vi[1..16] {
        *register = count.unwrap_or((next() % 32 + 1) as u16);
    }
    state.acc = std::array::from_fn(|_| float(&mut next));
    state.q = float(&mut next);
    state.p = float(&mut next);
    state.i = float(&mut next);
    state.top = (next() % 512) as u16;
    state.itop = (next() % 32 + 1) as u16;
    state
}
