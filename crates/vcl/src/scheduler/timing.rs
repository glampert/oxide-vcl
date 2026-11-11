//! Replay-validated removal of empty VU issue rows.
//!
//! The original emulates raw bundles, inserts synchronization
//! caps, and reduces empty rows.
//! This module reconstructs the safety principle with explicit producer
//! identities. It is conservative: it does not reproduce arbitrary cap moves,
//! sticky-flag algebra, external path timing, or every original search choice.
//! Hardware context is SCE VU User's Manual sections 3.4.1, 3.4.3, 3.4.6,
//! and 3.4.8: VF/loaded-VI interlock, P/Q/ACC/flags do not, and stalls can change
//! which noninterlocked producer a read observes.

use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Synchronization {
    Q {
        register: usize,
    },
    P {
        register: usize,
    },
    /// ABS into VF00, whose ignored result turns a real VF input hazard into
    /// an interlock before a noninterlocked status/MAC flag read.
    Vector {
        register: usize,
        components: u8,
        producer: usize,
    },
}

impl Synchronization {
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Q { .. } => "WAITQ",
            Self::P { .. } => "WAITP",
            Self::Vector { .. } => "ABS",
        }
    }

    pub const fn register(self) -> usize {
        match self {
            Self::Q { register } | Self::P { register } | Self::Vector { register, .. } => register,
        }
    }

    pub const fn issue_slot(self) -> usize {
        match self {
            Self::Vector { .. } => 0,
            Self::P { .. } | Self::Q { .. } => 1,
        }
    }

    const fn early_release(self) -> u32 {
        // Recovered cap/reduction code uses one less cycle for P than Q.
        match self {
            Self::Q { .. } | Self::Vector { .. } => 0,
            Self::P { .. } => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingRead {
    pub register: usize,
    pub components: u8,
    /// None is the externally supplied value before this straight-line replay.
    pub producer: Option<usize>,
    /// VF and interlocked VI use true; ACC/I/P/Q/R/flags normally use false.
    /// An explicit WAIT can instead be represented by row synchronization.
    pub interlocked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingWrite {
    pub register: usize,
    pub components: u8,
    pub latency: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimingOperation {
    pub reads: Vec<TimingRead>,
    pub writes: Vec<TimingWrite>,
    pub unit_mask: u16,
    pub throughput: u16,
    /// P/Q or noninterlocked flag consumers eligible for optional cap
    /// insertion. A Vector cap names a still-valid VF result of the expected
    /// flag producer. IDs belong to the caller's physical register namespace;
    /// no global numeric-name assumption is made.
    pub wait_reads: Vec<Synchronization>,
}

impl Default for TimingOperation {
    fn default() -> Self {
        Self {
            reads: Vec::new(),
            writes: Vec::new(),
            unit_mask: 0,
            throughput: 1,
            wait_reads: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimedRow {
    pub instructions: [Option<usize>; 2],
    /// May also describe a preexisting lower WAIT instruction. Automatic caps
    /// use an empty lower slot for P/Q or an empty upper slot for Vector.
    pub synchronization: Option<Synchronization>,
    /// End-bit rows, branch delay slots, boundaries, and explicit timing fences.
    pub protected: bool,
}

impl TimedRow {
    pub fn new(instructions: [Option<usize>; 2]) -> Self {
        Self {
            instructions,
            synchronization: None,
            protected: false,
        }
    }

    fn empty(&self) -> bool {
        self.instructions.iter().all(Option::is_none) && self.synchronization.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingOutput {
    pub register: usize,
    pub components: u8,
    pub producer: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stalls {
    pub latency: u32,
    pub throughput: u32,
    pub synchronization: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replay {
    /// Actual issue time after all hardware stalls, one entry per physical row.
    pub issue_times: Vec<u32>,
    pub stalls: Vec<Stalls>,
    /// Processor time immediately after the last row, excluding explicit drain.
    pub elapsed: u32,
    /// Extra idle cycles needed before the specified outputs are all visible.
    /// These cycles must be emitted or guaranteed by the caller at CFG exits.
    pub drain_cycles: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compressed {
    pub rows: Vec<TimedRow>,
    /// Original physical row -> new physical row; removed rows map to None.
    pub row_map: Vec<Option<usize>>,
    pub replay: Replay,
    pub removed: usize,
    pub inserted_waits: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimingError {
    MissingOperation {
        row: usize,
        operation: usize,
    },
    DuplicateOperation(usize),
    InvalidComponents(u8),
    InvalidProducer(usize),
    ZeroThroughput(usize),
    ReadMismatch {
        row: usize,
        operation: usize,
        register: usize,
        component: u8,
        expected: Option<usize>,
        observed: Option<usize>,
    },
    CapReadMismatch {
        row: usize,
        register: usize,
        component: u8,
        expected: usize,
        observed: Option<usize>,
    },
    OutputMismatch {
        register: usize,
        component: u8,
        expected: Option<usize>,
        observed: Option<usize>,
    },
    WriteCollision {
        first: usize,
        second: usize,
        register: usize,
        cycle: u32,
    },
    TimeOverflow,
}

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VU timing replay failed: {self:?}")
    }
}

impl std::error::Error for TimingError {}

#[derive(Clone, Copy)]
struct Event {
    writer: usize,
    register: usize,
    components: u8,
    ready: u32,
}

fn components(mask: u8) -> impl Iterator<Item = u8> {
    (0..7).filter(move |&part| mask & (1 << part) != 0)
}

fn valid_mask(mask: u8) -> Result<(), TimingError> {
    if mask == 0 || mask & 0x80 != 0 {
        Err(TimingError::InvalidComponents(mask))
    } else {
        Ok(())
    }
}

fn value_at(events: &[Event], register: usize, component: u8, time: u32) -> Option<usize> {
    events
        .iter()
        .filter(|event| {
            event.register == register
                && event.components & (1 << component) != 0
                && event.ready <= time
        })
        .max_by_key(|event| event.ready)
        .map(|event| event.writer)
}

fn latest_ready(events: &[Event], register: usize, mask: u8) -> u32 {
    events
        .iter()
        .filter(|event| event.register == register && event.components & mask != 0)
        .map(|event| event.ready)
        .max()
        .unwrap_or(0)
}

/// Replay one straight-line physical issue stream, including both pipes.
///
/// Register IDs must already express physical aliases, and reads must retain
/// their expected value definitions. Both pipes' reads occur before either
/// pipe's new writes, matching the original. The caller supplies branch/fence
/// boundaries and handles control-flow joins; flattening a CFG into this API
/// does not establish safe interstream timing.
pub fn replay(
    rows: &[TimedRow],
    operations: &[TimingOperation],
    outputs: &[TimingOutput],
) -> Result<Replay, TimingError> {
    validate_stream(rows, operations, outputs)?;

    let mut state = ReplayState {
        events: Vec::new(),
        units: [0u32; 16],
        time: 0u32,
    };
    let mut issue_times = Vec::with_capacity(rows.len());
    let mut stalls = Vec::with_capacity(rows.len());

    for (row_index, row) in rows.iter().enumerate() {
        let row_stalls = state.stall_row(row, operations);
        state.check_row_reads(row_index, row, operations)?;
        issue_times.push(state.time);
        stalls.push(row_stalls);
        state.issue_row(row, operations)?;
    }

    let time = state.time;
    let output_time = state.settle_outputs(outputs);
    state.check_outputs(outputs, output_time)?;
    Ok(Replay {
        issue_times,
        stalls,
        elapsed: time,
        drain_cycles: output_time - time,
    })
}

/// Check that rows, operations and outputs are well formed for a replay.
fn validate_stream(
    rows: &[TimedRow],
    operations: &[TimingOperation],
    outputs: &[TimingOutput],
) -> Result<(), TimingError> {
    let mut seen = BTreeSet::new();

    for (row_index, row) in rows.iter().enumerate() {
        if let Some(Synchronization::Vector {
            components,
            producer,
            ..
        }) = row.synchronization
        {
            valid_mask(components)?;
            if producer >= operations.len() {
                return Err(TimingError::InvalidProducer(producer));
            }
        }
        for &operation in row.instructions.iter().flatten() {
            let Some(operation_data) = operations.get(operation) else {
                return Err(TimingError::MissingOperation {
                    row: row_index,
                    operation,
                });
            };
            if !seen.insert(operation) {
                return Err(TimingError::DuplicateOperation(operation));
            }
            if operation_data.throughput == 0 {
                return Err(TimingError::ZeroThroughput(operation));
            }
            for read in &operation_data.reads {
                valid_mask(read.components)?;
                if read
                    .producer
                    .is_some_and(|producer| producer >= operations.len())
                {
                    return Err(TimingError::InvalidProducer(read.producer.unwrap()));
                }
            }
            for write in &operation_data.writes {
                valid_mask(write.components)?;
            }
        }
    }

    for output in outputs {
        valid_mask(output.components)?;
        if output
            .producer
            .is_some_and(|producer| producer >= operations.len())
        {
            return Err(TimingError::InvalidProducer(output.producer.unwrap()));
        }
    }
    Ok(())
}

/// Running state of a replay: pending write events, unit availability and
/// the processor time.
struct ReplayState {
    events: Vec<Event>,
    units: [u32; 16],
    time: u32,
}

impl ReplayState {
    /// Advance the time past the throughput, latency and synchronization
    /// stalls of a row, returning how long each one lasted.
    fn stall_row(&mut self, row: &TimedRow, operations: &[TimingOperation]) -> Stalls {
        let mut row_stalls = Stalls::default();
        let mut ready = self.time;

        for &operation in row.instructions.iter().flatten() {
            let operation = &operations[operation];
            for (unit, &available) in self.units.iter().enumerate() {
                if operation.unit_mask & (1 << unit) != 0 {
                    ready = ready.max(available);
                }
            }
        }
        row_stalls.throughput = ready - self.time;
        self.time = ready;

        for &operation in row.instructions.iter().flatten() {
            for read in operations[operation]
                .reads
                .iter()
                .filter(|read| read.interlocked)
            {
                ready = ready.max(latest_ready(&self.events, read.register, read.components));
            }
        }
        if let Some(Synchronization::Vector {
            register,
            components,
            ..
        }) = row.synchronization
        {
            ready = ready.max(latest_ready(&self.events, register, components));
        }
        row_stalls.latency = ready - self.time;
        self.time = ready;

        if let Some(sync) = row.synchronization
            && !matches!(sync, Synchronization::Vector { .. })
        {
            ready = ready.max(
                latest_ready(&self.events, sync.register(), 0x7f)
                    .saturating_sub(sync.early_release()),
            );
        }
        row_stalls.synchronization = ready - self.time;
        self.time = ready;
        row_stalls
    }

    /// Check that a Vector cap and every read of the row see the value
    /// definition they expect at the current time.
    fn check_row_reads(
        &self,
        row_index: usize,
        row: &TimedRow,
        operations: &[TimingOperation],
    ) -> Result<(), TimingError> {
        let time = self.time;

        if let Some(Synchronization::Vector {
            register,
            components: mask,
            producer,
        }) = row.synchronization
        {
            for component in components(mask) {
                let observed = value_at(&self.events, register, component, time);
                if observed != Some(producer) {
                    return Err(TimingError::CapReadMismatch {
                        row: row_index,
                        register,
                        component,
                        expected: producer,
                        observed,
                    });
                }
            }
        }

        // Completion of older pending writes during a stall is visible here.
        // Comparing producer identities catches timing-sensitive flag changes.
        for &operation_index in row.instructions.iter().flatten() {
            for read in &operations[operation_index].reads {
                for component in components(read.components) {
                    let observed = value_at(&self.events, read.register, component, time);
                    if observed != read.producer {
                        return Err(TimingError::ReadMismatch {
                            row: row_index,
                            operation: operation_index,
                            register: read.register,
                            component,
                            expected: read.producer,
                            observed,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Record the writes and unit occupancy of an issued row and step the
    /// time past it.
    fn issue_row(
        &mut self,
        row: &TimedRow,
        operations: &[TimingOperation],
    ) -> Result<(), TimingError> {
        let time = self.time;

        for &operation_index in row.instructions.iter().flatten() {
            let operation = &operations[operation_index];
            for write in &operation.writes {
                let completion = time
                    .checked_add(u32::from(write.latency))
                    .ok_or(TimingError::TimeOverflow)?;
                // The hardware gives upper writes priority for an entire
                // register, even on disjoint components. Reject such collisions
                // conservatively instead of losing a lower result silently.
                if let Some(previous) = self.events.iter().find(|event| {
                    event.register == write.register
                        && event.ready == completion
                        && event.writer != operation_index
                }) {
                    return Err(TimingError::WriteCollision {
                        first: previous.writer,
                        second: operation_index,
                        register: write.register,
                        cycle: completion,
                    });
                }
                self.events.push(Event {
                    writer: operation_index,
                    register: write.register,
                    components: write.components,
                    ready: completion,
                });
            }
            let available = time
                .checked_add(u32::from(operation.throughput))
                .ok_or(TimingError::TimeOverflow)?;
            for (unit, deadline) in self.units.iter_mut().enumerate() {
                if operation.unit_mask & (1 << unit) != 0 {
                    *deadline = (*deadline).max(available);
                }
            }
        }
        self.time = time.checked_add(1).ok_or(TimingError::TimeOverflow)?;
        Ok(())
    }

    /// The time when every pending write to an output has landed.
    fn settle_outputs(&self, outputs: &[TimingOutput]) -> u32 {
        let mut output_time = self.time;

        for output in outputs {
            // All pending writes to an escaping physical value must settle before
            // the next separately replayed block can assume a stable input value.
            output_time = output_time.max(latest_ready(
                &self.events,
                output.register,
                output.components,
            ));
            for component in components(output.components) {
                if let Some(producer) = output.producer
                    && let Some(event) = self
                        .events
                        .iter()
                        .filter(|event| {
                            event.writer == producer
                                && event.register == output.register
                                && event.components & (1 << component) != 0
                        })
                        .max_by_key(|event| event.ready)
                {
                    output_time = output_time.max(event.ready);
                }
            }
        }
        output_time
    }

    /// Check that every output holds its expected producer at `output_time`.
    fn check_outputs(&self, outputs: &[TimingOutput], output_time: u32) -> Result<(), TimingError> {
        for output in outputs {
            for component in components(output.components) {
                let observed = value_at(&self.events, output.register, component, output_time);
                if observed != output.producer {
                    return Err(TimingError::OutputMismatch {
                        register: output.register,
                        component,
                        expected: output.producer,
                        observed,
                    });
                }
            }
        }
        Ok(())
    }
}

fn reduce_empty_rows(
    rows: &mut Vec<TimedRow>,
    origins: &mut Vec<usize>,
    operations: &[TimingOperation],
    outputs: &[TimingOutput],
    maximum_drain: u32,
) {
    let mut index = 1;
    while index + 1 < rows.len() {
        if rows[index].protected || !rows[index].empty() {
            index += 1;
            continue;
        }
        let saved = rows.remove(index);
        let safe = replay(rows, operations, outputs)
            .is_ok_and(|result| result.drain_cycles <= maximum_drain);
        if safe {
            origins.remove(index);
        } else {
            rows.insert(index, saved);
            index += 1;
        }
    }
}

/// Remove empty rows only after replay proves every read sees its original
/// definition. Never removes first/last/protected rows or increases output
/// drain debt. Optional P/Q caps use existing free lower slots; Vector caps use
/// free upper slots and validate that every capped VF component still holds
/// the named producer's result. A Vector cap is an ABS into discarded VF00;
/// it adds no write events and does not change the flag value being read.
///
/// End-bit placement is the caller's responsibility: mark its row protected
/// before calling. This function never reproduces the original fixture-specific
/// E-loss anomaly by deleting a required end marker.
pub fn compress(
    rows: &[TimedRow],
    operations: &[TimingOperation],
    outputs: &[TimingOutput],
    insert_waits: bool,
) -> Result<Compressed, TimingError> {
    let baseline = replay(rows, operations, outputs)?;
    let mut reduced = rows.to_vec();
    let mut origins: Vec<_> = (0..rows.len()).collect();
    let mut inserted_waits = 0;

    if insert_waits {
        let context = WaitContext {
            operations,
            outputs,
            maximum_drain: baseline.drain_cycles,
        };
        // Try consumers in physical order. The latest available compatible
        // slot can cap a producer without adding a row. A P cap releases one
        // cycle before P availability, so the next MFP can observe the result.
        for (original_consumer, original_row) in rows.iter().enumerate() {
            for &operation_index in original_row.instructions.iter().flatten() {
                let waits: BTreeSet<_> = operations[operation_index]
                    .wait_reads
                    .iter()
                    .copied()
                    .collect();
                for wait in waits {
                    let inserted = context.try_insert_wait(
                        &mut reduced,
                        &mut origins,
                        original_consumer,
                        operation_index,
                        wait,
                    )?;
                    if inserted {
                        inserted_waits += 1;
                    }
                }
            }
        }
    }

    reduce_empty_rows(
        &mut reduced,
        &mut origins,
        operations,
        outputs,
        baseline.drain_cycles,
    );
    let replay = replay(&reduced, operations, outputs)?;
    let mut row_map = vec![None; rows.len()];
    for (new, &old) in origins.iter().enumerate() {
        row_map[old] = Some(new);
    }
    Ok(Compressed {
        removed: rows.len() - reduced.len(),
        rows: reduced,
        row_map,
        replay,
        inserted_waits,
    })
}

/// Inputs shared by every optional cap attempt of one compression.
struct WaitContext<'a> {
    operations: &'a [TimingOperation],
    outputs: &'a [TimingOutput],
    /// Output drain debt of the uncompressed stream.
    maximum_drain: u32,
}

impl WaitContext<'_> {
    /// Try to cap the producer of one wait read of an operation, keeping the
    /// cap (and the rows it frees) when that shortens the stream or leaves a
    /// vector gap. Returns whether a cap was kept.
    fn try_insert_wait(
        &self,
        reduced: &mut Vec<TimedRow>,
        origins: &mut Vec<usize>,
        original_consumer: usize,
        operation_index: usize,
        wait: Synchronization,
    ) -> Result<bool, TimingError> {
        let (operations, outputs) = (self.operations, self.outputs);
        let Some(consumer) = origins
            .iter()
            .position(|&origin| origin == original_consumer)
        else {
            return Ok(false);
        };
        let producer = if let Synchronization::Vector { producer, .. } = wait {
            Some(producer)
        } else {
            operations[operation_index]
                .reads
                .iter()
                .find(|read| read.register == wait.register())
                .and_then(|read| read.producer)
        };
        let Some(producer) = producer else {
            return Ok(false);
        };
        let Some(producer_row) = reduced
            .iter()
            .position(|row| row.instructions.contains(&Some(producer)))
        else {
            return Ok(false);
        };

        let current = replay(reduced, operations, outputs)?;
        let mask = match wait {
            Synchronization::Vector { components, .. } => components,
            Synchronization::Q { .. } | Synchronization::P { .. } => 0x7f,
        };
        let Some(latency) = operations[producer]
            .writes
            .iter()
            .filter(|write| write.register == wait.register() && write.components & mask != 0)
            .map(|write| u32::from(write.latency))
            .max()
        else {
            return Ok(false);
        };
        let available = current.issue_times[producer_row]
            .checked_add(latency)
            .ok_or(TimingError::TimeOverflow)?
            .saturating_sub(wait.early_release());
        let Some(cap_row) = cap_row(reduced, &current, wait, producer_row, consumer, available)
        else {
            return Ok(false);
        };

        let mut candidate = reduced.clone();
        candidate[cap_row].synchronization = Some(wait);
        if replay(&candidate, operations, outputs).is_err() {
            return Ok(false);
        }
        let mut candidate_origins = origins.clone();
        reduce_empty_rows(
            &mut candidate,
            &mut candidate_origins,
            operations,
            outputs,
            self.maximum_drain,
        );

        // The original cap pass runs before row reduction. A
        // vector cap may remain even when a second status resource
        // requires the explicit gap (for example ALU+DIV→FSEQ).
        let vector_gap = self.vector_gap(reduced, wait, producer, producer_row, consumer);
        if candidate.len() < reduced.len() || vector_gap {
            *reduced = candidate;
            *origins = candidate_origins;
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether a Vector cap's producer latency is not covered by the nonempty
    /// rows before the consumer while an empty row still separates them.
    fn vector_gap(
        &self,
        reduced: &[TimedRow],
        wait: Synchronization,
        producer: usize,
        producer_row: usize,
        consumer: usize,
    ) -> bool {
        let Synchronization::Vector {
            register,
            components,
            ..
        } = wait
        else {
            return false;
        };
        let latency = self.operations[producer]
            .writes
            .iter()
            .filter(|write| write.register == register && write.components & components != 0)
            .map(|write| usize::from(write.latency))
            .max()
            .unwrap_or(0);
        reduced[producer_row + 1..=consumer]
            .iter()
            .filter(|row| !row.empty())
            .count()
            < latency
            && reduced[producer_row + 1..consumer]
                .iter()
                .any(TimedRow::empty)
    }
}

/// The row to hold a cap between a producer and its consumer.
///
/// The original keeps the latest candidate sharing a nonempty pair. With no
/// such pair it keeps the first eligible empty row, rather than the latest
/// empty row.
fn cap_row(
    reduced: &[TimedRow],
    current: &Replay,
    wait: Synchronization,
    producer_row: usize,
    consumer: usize,
    available: u32,
) -> Option<usize> {
    let mut empty_slot = None;
    let mut paired_slot = None;

    for (index, row) in reduced
        .iter()
        .enumerate()
        .take(consumer + 1)
        .skip(producer_row + 1)
    {
        if current.issue_times[index] < available
            || row.instructions[wait.issue_slot()].is_some()
            || row.synchronization.is_some()
            || row.protected
        {
            continue;
        }
        if row.instructions.iter().any(Option::is_some) {
            paired_slot = Some(index);
        } else {
            empty_slot.get_or_insert(index);
        }
    }
    paired_slot.or(empty_slot)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::instruction::unit;

    fn write(register: usize, latency: u16) -> TimingOperation {
        TimingOperation {
            writes: vec![TimingWrite {
                register,
                components: 1,
                latency,
            }],
            ..Default::default()
        }
    }

    fn read(register: usize, producer: Option<usize>, interlocked: bool) -> TimingOperation {
        TimingOperation {
            reads: vec![TimingRead {
                register,
                components: 1,
                producer,
                interlocked,
            }],
            ..Default::default()
        }
    }

    fn empty() -> TimedRow {
        TimedRow::new([None, None])
    }

    #[test]
    fn vf_latency_becomes_a_hardware_stall_after_compaction() {
        let operations = [write(1, 4), read(1, Some(0), true)];
        let rows = [
            TimedRow::new([Some(0), None]),
            empty(),
            empty(),
            empty(),
            TimedRow::new([Some(1), None]),
            empty(),
        ];
        let result = compress(&rows, &operations, &[], false).unwrap();
        assert_eq!(result.removed, 3);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.replay.issue_times, [0, 4, 5]);
        assert_eq!(result.replay.stalls[1].latency, 3);
        assert_eq!(
            result.row_map,
            [Some(0), None, None, None, Some(1), Some(2)]
        );
    }

    #[test]
    fn noninterlocked_acc_read_retains_required_delay() {
        let operations = [write(48, 4), read(48, Some(0), false)];
        let rows = [
            TimedRow::new([Some(0), None]),
            empty(),
            empty(),
            empty(),
            TimedRow::new([Some(1), None]),
        ];
        let result = compress(&rows, &operations, &[], false).unwrap();
        assert_eq!(result.removed, 0);
    }

    #[test]
    fn every_component_has_its_own_completion_and_definition() {
        let operations = [
            TimingOperation {
                writes: vec![
                    TimingWrite {
                        register: 1,
                        components: 1,
                        latency: 1,
                    },
                    TimingWrite {
                        register: 1,
                        components: 2,
                        latency: 4,
                    },
                ],
                ..Default::default()
            },
            TimingOperation {
                reads: vec![TimingRead {
                    register: 1,
                    components: 2,
                    producer: Some(0),
                    interlocked: true,
                }],
                ..Default::default()
            },
        ];
        let result = replay(
            &[
                TimedRow::new([Some(0), None]),
                TimedRow::new([Some(1), None]),
            ],
            &operations,
            &[],
        )
        .unwrap();
        assert_eq!(result.issue_times, [0, 4]);
    }

    #[test]
    fn a_stall_cannot_change_the_flag_writer_seen_by_a_read() {
        let mut consumer = read(55, Some(0), false);
        consumer.reads.push(TimingRead {
            register: 1,
            components: 1,
            producer: Some(1),
            interlocked: true,
        });
        let operations = [write(55, 4), write(1, 8), write(55, 4), consumer];
        let rows = [
            TimedRow::new([Some(0), None]),
            TimedRow::new([Some(1), None]),
            empty(),
            empty(),
            empty(),
            empty(),
            TimedRow::new([Some(2), None]),
            TimedRow::new([Some(3), None]),
        ];
        assert_eq!(
            replay(&rows, &operations, &[]).unwrap().issue_times.last(),
            Some(&9)
        );
        // Removing any delay before the second flag setter makes that setter
        // complete during the VF-induced stall, changing the status read.
        assert_eq!(compress(&rows, &operations, &[], false).unwrap().removed, 0);
    }

    #[test]
    fn both_bundle_reads_precede_new_writes_even_at_zero_latency() {
        let operations = [write(1, 0), read(1, None, true)];
        assert!(replay(&[TimedRow::new([Some(0), Some(1)])], &operations, &[]).is_ok());
    }

    #[test]
    fn simultaneous_disjoint_register_writes_are_rejected() {
        let mut second = write(1, 4);
        second.writes[0].components = 2;
        let operations = [write(1, 4), second];
        let rows = [TimedRow::new([Some(0), Some(1)])];
        let expected = TimingError::WriteCollision {
            first: 0,
            second: 1,
            register: 1,
            cycle: 4,
        };
        assert_eq!(replay(&rows, &operations, &[]), Err(expected.clone()));
        // Compression validates its input before considering caps or deleting
        // rows. An invalid allocated bundle cannot become an accepted baseline.
        assert_eq!(
            compress(&rows, &operations, &[], true).unwrap_err(),
            expected
        );
    }

    #[test]
    fn xyz_and_w_write_ports_can_share_a_pair_when_registers_differ() {
        // Original timing-collision-packed-webs pairs ADD.xyz VF02 with
        // MFIR.w VF03. Component packing does not make them one write port.
        let mut upper = write(2, 4);
        upper.writes[0].components = 7;
        let mut lower = write(3, 4);
        lower.writes[0].components = 8;
        let outputs = [
            TimingOutput {
                register: 2,
                components: 7,
                producer: Some(0),
            },
            TimingOutput {
                register: 3,
                components: 8,
                producer: Some(1),
            },
        ];
        let result = replay(
            &[TimedRow::new([Some(0), Some(1)])],
            &[upper, lower],
            &outputs,
        )
        .unwrap();
        assert_eq!(result.issue_times, [0]);
        assert_eq!(result.drain_cycles, 3);
    }

    #[test]
    fn xyz_and_w_writes_to_one_register_are_safe_on_consecutive_pairs() {
        // Original timing-collision-same-web separates ADD.xyz and MFIR.w,
        // although both values are components of the same escaping VF02.
        let mut upper = write(2, 4);
        upper.writes[0].components = 7;
        let mut lower = write(2, 4);
        lower.writes[0].components = 8;
        let outputs = [
            TimingOutput {
                register: 2,
                components: 7,
                producer: Some(0),
            },
            TimingOutput {
                register: 2,
                components: 8,
                producer: Some(1),
            },
        ];
        let result = replay(
            &[
                TimedRow::new([Some(0), None]),
                TimedRow::new([None, Some(1)]),
            ],
            &[upper, lower],
            &outputs,
        )
        .unwrap();
        assert_eq!(result.issue_times, [0, 1]);
        assert_eq!(result.drain_cycles, 3);
    }

    #[test]
    fn register_write_port_exclusion_does_not_broaden_component_read_interlocks() {
        // The whole-register write collision rule must not turn a pending W
        // write into a dependency for an unrelated external XYZ input.
        let mut lower = write(2, 4);
        lower.writes[0].components = 8;
        let mut consumer = read(2, None, true);
        consumer.reads[0].components = 7;
        let result = replay(
            &[
                TimedRow::new([None, Some(0)]),
                TimedRow::new([Some(1), None]),
            ],
            &[lower, consumer],
            &[],
        )
        .unwrap();
        assert_eq!(result.issue_times, [0, 1]);
        assert_eq!(result.stalls[1].latency, 0);
    }

    #[test]
    fn p_wait_releases_a_cycle_before_the_following_read() {
        let operations = [write(52, 8), read(52, Some(0), false)];
        let mut wait = empty();
        wait.synchronization = Some(Synchronization::P { register: 52 });
        let rows = [
            TimedRow::new([None, Some(0)]),
            wait,
            TimedRow::new([None, Some(1)]),
        ];
        let result = replay(&rows, &operations, &[]).unwrap();
        assert_eq!(result.issue_times, [0, 7, 8]);
        assert_eq!(result.stalls[1].synchronization, 6);
    }

    #[test]
    fn optional_q_cap_shares_the_upper_consumers_row() {
        let mut consumer = read(50, Some(0), false);
        consumer
            .wait_reads
            .push(Synchronization::Q { register: 50 });
        let operations = [write(50, 7), consumer];
        let mut rows = vec![TimedRow::new([None, Some(0)])];
        rows.extend((0..6).map(|_| empty()));
        rows.push(TimedRow::new([Some(1), None]));
        rows.push(empty());
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 1);
        assert_eq!(result.removed, 6);
        assert_eq!(
            result.rows[1].synchronization,
            Some(Synchronization::Q { register: 50 })
        );
        assert_eq!(result.replay.issue_times[1], 7);
    }

    #[test]
    fn optional_p_cap_uses_the_preceding_lower_slot() {
        let mut consumer = read(52, Some(0), false);
        consumer
            .wait_reads
            .push(Synchronization::P { register: 52 });
        let operations = [write(52, 8), consumer];
        let mut rows = vec![TimedRow::new([None, Some(0)])];
        rows.extend((0..7).map(|_| empty()));
        rows.push(TimedRow::new([None, Some(1)]));
        rows.push(empty());
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 1);
        assert_eq!(
            result.rows[1].synchronization,
            Some(Synchronization::P { register: 52 })
        );
        assert_eq!(result.replay.issue_times[2], 8);
    }

    fn flag_producer(vector_register: usize, flag_register: usize) -> TimingOperation {
        TimingOperation {
            writes: vec![
                TimingWrite {
                    register: vector_register,
                    components: 15,
                    latency: 4,
                },
                TimingWrite {
                    register: flag_register,
                    components: 1,
                    latency: 4,
                },
            ],
            unit_mask: 1,
            ..Default::default()
        }
    }

    fn flag_consumer(vector_register: usize, flag_register: usize, mask: u8) -> TimingOperation {
        let mut consumer = read(flag_register, Some(0), false);
        consumer.wait_reads.push(Synchronization::Vector {
            register: vector_register,
            components: mask,
            producer: 0,
        });
        consumer
    }

    #[test]
    fn vector_cap_turns_flag_latency_into_a_same_pair_vf_interlock() {
        // Mirrors original hw-fsand-after-alu: ADD sets VF02 and STAT_ALU;
        // FSAND cannot interlock on STAT_ALU. ABS VF00,VF02 in the upper slot
        // stalls the whole pair until both results from ADD are available.
        let operations = [flag_producer(2, 55), flag_consumer(2, 55, 15)];
        let rows = [
            TimedRow::new([Some(0), None]),
            empty(),
            empty(),
            empty(),
            TimedRow::new([None, Some(1)]),
        ];
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.removed, 3);
        assert_eq!(result.inserted_waits, 1);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[1].instructions, [None, Some(1)]);
        assert_eq!(
            result.rows[1].synchronization,
            Some(Synchronization::Vector {
                register: 2,
                components: 15,
                producer: 0,
            })
        );
        assert_eq!(result.replay.issue_times, [0, 4]);
        assert_eq!(result.replay.stalls[1].latency, 3);
        assert_eq!(result.replay.stalls[1].synchronization, 0);
    }

    #[test]
    fn vector_cap_never_replaces_a_real_upper_instruction_or_a_protected_gap() {
        let operations = [
            flag_producer(2, 55),
            flag_consumer(2, 55, 15),
            TimingOperation::default(),
        ];
        let mut protected = empty();
        protected.protected = true;
        let rows = [
            TimedRow::new([Some(0), None]),
            protected.clone(),
            protected.clone(),
            protected,
            TimedRow::new([Some(2), Some(1)]),
        ];
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 0);
        assert_eq!(result.removed, 0);
        assert_eq!(result.rows[4].instructions, [Some(2), Some(1)]);
    }

    #[test]
    fn vector_cap_checks_that_a_later_write_did_not_replace_its_source() {
        let mut cap = empty();
        cap.synchronization = Some(Synchronization::Vector {
            register: 2,
            components: 1,
            producer: 0,
        });
        let operations = [write(2, 4), write(2, 4)];
        let rows = [
            TimedRow::new([Some(0), None]),
            TimedRow::new([Some(1), None]),
            cap,
        ];
        assert!(matches!(
            replay(&rows, &operations, &[]),
            Err(TimingError::CapReadMismatch {
                row: 2,
                register: 2,
                component: 0,
                expected: 0,
                observed: Some(1),
            })
        ));
    }

    #[test]
    fn a_vector_cap_can_use_components_untouched_by_a_later_partial_write() {
        let mut partial = write(2, 1);
        partial.writes[0].components = 2;
        let operations = [flag_producer(2, 55), partial, flag_consumer(2, 55, 1)];
        let rows = [
            TimedRow::new([Some(0), None]),
            TimedRow::new([Some(1), None]),
            empty(),
            empty(),
            TimedRow::new([None, Some(2)]),
        ];
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 1);
        assert_eq!(result.removed, 2);
        assert_eq!(result.replay.issue_times, [0, 1, 4]);
        assert_eq!(result.replay.stalls[2].latency, 2);
    }

    #[test]
    fn a_vector_cap_is_rejected_if_its_stall_changes_the_flag_producer() {
        let mut producer = flag_producer(2, 55);
        producer.writes[0].latency = 8;
        let operations = [producer, write(55, 4), flag_consumer(2, 55, 15)];
        let rows = [
            TimedRow::new([Some(0), None]),
            TimedRow::new([Some(1), None]),
            empty(),
            empty(),
            TimedRow::new([None, Some(2)]),
        ];
        // At cycle 4 the flag still belongs to operation 0. Waiting for VF02
        // through cycle 8 would observe operation 1's flag instead.
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 0);
        assert_eq!(result.removed, 0);
        assert!(result.rows.iter().all(|row| row.synchronization.is_none()));
    }

    #[test]
    fn vector_cap_requires_a_valid_mask_and_producer_identity() {
        let mut cap = empty();
        cap.synchronization = Some(Synchronization::Vector {
            register: 2,
            components: 0,
            producer: 0,
        });
        assert_eq!(
            replay(&[cap.clone()], &[write(2, 4)], &[]),
            Err(TimingError::InvalidComponents(0))
        );
        cap.synchronization = Some(Synchronization::Vector {
            register: 2,
            components: 1,
            producer: 1,
        });
        assert_eq!(
            replay(&[cap], &[write(2, 4)], &[]),
            Err(TimingError::InvalidProducer(1))
        );
    }

    #[test]
    fn cap_selection_prefers_a_nonempty_pair_over_later_empty_rows() {
        let operations = [
            flag_producer(2, 55),
            TimingOperation::default(),
            TimingOperation::default(),
            flag_consumer(2, 55, 15),
        ];
        let rows = [
            TimedRow::new([Some(0), None]),
            empty(),
            empty(),
            empty(),
            TimedRow::new([None, Some(1)]),
            TimedRow::new([None, Some(2)]),
            empty(),
            empty(),
            TimedRow::new([Some(3), None]),
        ];
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 1);
        let paired = result.row_map[5].unwrap();
        assert!(matches!(
            result.rows[paired].synchronization,
            Some(Synchronization::Vector { .. })
        ));
        assert_eq!(result.rows[paired].instructions, [None, Some(2)]);
        assert!(result.row_map[6].is_none());
        assert!(result.row_map[7].is_none());
    }

    #[test]
    fn cap_selection_does_not_use_a_pair_before_original_availability() {
        let operations = [
            flag_producer(2, 55),
            TimingOperation::default(),
            flag_consumer(2, 55, 15),
        ];
        let mut protected = empty();
        protected.protected = true;
        let rows = [
            TimedRow::new([Some(0), None]),
            empty(),
            TimedRow::new([None, Some(1)]),
            protected.clone(),
            protected,
            TimedRow::new([Some(2), None]),
        ];
        // Row 2 has a compatible upper slot but its baseline timestamp is
        // before the vector/flag result's cycle 4 availability. All later
        // compatible empty slots are protected, so no cap is eligible.
        let result = compress(&rows, &operations, &[], true).unwrap();
        assert_eq!(result.inserted_waits, 0);
    }

    #[test]
    fn end_marker_and_delay_slot_rows_are_protected() {
        let mut end = empty();
        end.protected = true;
        let rows = [TimedRow::new([Some(0), None]), empty(), end, empty()];
        let result = compress(&rows, &[TimingOperation::default()], &[], false).unwrap();
        assert_eq!(result.removed, 1);
        assert_eq!(result.row_map[2], Some(1));
        assert!(result.rows[1].protected);
    }

    #[test]
    fn output_drain_debt_does_not_increase_after_compression() {
        let operations = [write(1, 4)];
        let rows = [TimedRow::new([Some(0), None]), empty(), empty(), empty()];
        let outputs = [TimingOutput {
            register: 1,
            components: 1,
            producer: Some(0),
        }];
        assert_eq!(
            replay(&rows, &operations, &outputs).unwrap().drain_cycles,
            0
        );
        let result = compress(&rows, &operations, &outputs, false).unwrap();
        assert_eq!(result.removed, 0);
        let short = replay(&rows[..1], &operations, &outputs).unwrap();
        assert_eq!(short.drain_cycles, 3);
    }

    #[test]
    fn throughput_stalls_both_pipes() {
        let mut a = write(1, 1);
        a.unit_mask = unit::DIVIDE;
        a.throughput = 7;
        let mut b = write(2, 1);
        b.unit_mask = unit::DIVIDE;
        b.throughput = 7;
        let result = replay(
            &[
                TimedRow::new([None, Some(0)]),
                TimedRow::new([Some(2), Some(1)]),
            ],
            &[a, b, TimingOperation::default()],
            &[],
        )
        .unwrap();
        assert_eq!(result.issue_times, [0, 7]);
        assert_eq!(result.stalls[1].throughput, 6);
    }

    #[test]
    fn wrong_reaching_definition_is_a_clear_error() {
        let rows = [
            TimedRow::new([Some(0), None]),
            TimedRow::new([Some(1), None]),
        ];
        assert!(matches!(
            replay(&rows, &[write(1, 1), read(1, None, false)], &[]),
            Err(TimingError::ReadMismatch {
                expected: None,
                observed: Some(0),
                ..
            })
        ));
    }

    #[test]
    fn empty_stream_replays_and_compresses() {
        let result = compress(&[], &[], &[], true).unwrap();
        assert!(result.rows.is_empty());
        assert_eq!(result.replay.elapsed, 0);
    }
}
