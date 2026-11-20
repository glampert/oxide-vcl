//! Struct layouts in VU data memory: "VU packing" (`docs/VUSL.md`, Memory).
//!
//! Members go in order. A scalar takes one lane of a qword and a `vecN` N
//! consecutive lanes; a member that does not fit in the rest of the qword
//! starts the next one. A `vec4`, `qword`, matrix, struct or array starts a
//! qword of its own, and array elements each take whole qwords.
//! `layout(lane = L)` starts a member at lane L (of the next qword if the
//! current one is past L), and `layout(offset = Q)` at qword Q.
//!
//! An array's length may be read from an earlier `int16` member, or be the
//! count VIF ITOP sent, so what follows it sits at a run-time offset: a
//! fixed number of qwords plus that length times the array's stride. A
//! member with `layout(offset = Q)` after one sits at Q, and the arrays
//! before it must end by Q: a bound on their lengths. An open array (`T a[]`) can only be
//! last, and a struct ending in one only the last member of its parent.

use super::types::{Lane, Length, StructId, Type};
use crate::syntax::Pos;

/// A number of qwords: a fixed part, plus for each term the length read
/// from an `int16` member times a stride.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Extent {
    pub fixed: u32,
    pub terms: Vec<Term>,
}

/// One dependent length in an [`Extent`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Term {
    /// The `int16` member holding the length, by member indices from the
    /// struct the extent belongs to; no indices for the count XITOP reads.
    pub count: Vec<usize>,
    pub stride: u32,
}

impl Term {
    /// Whether the length is the count XITOP reads.
    pub fn is_xitop(&self) -> bool {
        self.count.is_empty()
    }
}

impl Extent {
    pub fn fixed(fixed: u32) -> Self {
        Self {
            fixed,
            terms: Vec::new(),
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn add(&mut self, other: &Extent) {
        self.fixed += other.fixed;
        for term in &other.terms {
            match self.terms.iter_mut().find(|own| own.count == term.count) {
                Some(own) => own.stride += term.stride,
                None => self.terms.push(term.clone()),
            }
        }
    }

    /// The extent of a nested struct's member, seen from the struct holding
    /// it as member `member`: its lengths are read through that member.
    pub fn nested(&self, member: usize) -> Extent {
        Extent {
            fixed: self.fixed,
            terms: self
                .terms
                .iter()
                .map(|term| Term {
                    // XITOP's count is the same from every struct.
                    count: if term.is_xitop() {
                        Vec::new()
                    } else {
                        std::iter::once(member)
                            .chain(term.count.iter().copied())
                            .collect()
                    },
                    stride: term.stride,
                })
                .collect(),
        }
    }
}

/// An open array at the end of a struct.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenTail {
    /// The array, by member indices from the struct.
    pub path: Vec<usize>,
    /// Its elements' size in qwords.
    pub stride: u32,
}

/// A VIF UNPACK format, recorded for the host (`layout(unpack = ...)`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unpack {
    /// Components per element: 1 (S), 2, 3 or 4 (V2, V3, V4, V4-5).
    pub components: u8,
    /// Bits per component: 32, 16 or 8; 5 for V4-5.
    pub bits: u8,
    /// Zero-extended rather than sign-extended (`unsigned`).
    pub unsigned: bool,
}

impl Unpack {
    pub fn from_name(name: &str) -> Option<Self> {
        let (components, bits) = match name {
            "s_32" => (1, 32),
            "s_16" => (1, 16),
            "s_8" => (1, 8),
            "v2_32" => (2, 32),
            "v2_16" => (2, 16),
            "v2_8" => (2, 8),
            "v3_32" => (3, 32),
            "v3_16" => (3, 16),
            "v3_8" => (3, 8),
            "v4_32" => (4, 32),
            "v4_16" => (4, 16),
            "v4_8" => (4, 8),
            "v4_5" => (4, 5),
            _ => return None,
        };
        Some(Self {
            components,
            bits,
            unsigned: false,
        })
    }

    /// The format's name: `v4_16`.
    pub fn name(&self) -> String {
        match self.components {
            1 => format!("s_{}", self.bits),
            n => format!("v{n}_{}", self.bits),
        }
    }
}

/// A struct with its members placed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub pos: Pos,
    pub members: Vec<MemberDef>,
    /// Its size, up to the open array if it ends in one.
    pub size: Extent,
    pub open: Option<OpenTail>,
    /// Where arrays whose lengths are read at run time must end, for the
    /// members at fixed offsets after them.
    pub bounds: Vec<Bound>,
}

/// Arrays whose lengths are read at run time that a member at a fixed
/// offset follows: they end at `end`, which must be at most `at`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bound {
    pub end: Extent,
    pub at: u32,
}

impl StructDef {
    pub fn member(&self, name: &str) -> Option<(usize, &MemberDef)> {
        self.members
            .iter()
            .enumerate()
            .find(|(_, member)| member.name == name)
    }
}

/// A placed member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberDef {
    pub name: String,
    pub ty: Type,
    /// Qwords from the struct's start.
    pub offset: Extent,
    /// The first lane, for members that take lanes rather than qwords.
    pub lane: Lane,
    pub unpack: Option<Unpack>,
    pub pos: Pos,
}

/// A member to place, as the checker resolved it.
#[derive(Clone, Debug)]
pub struct MemberSpec {
    pub name: String,
    pub ty: Type,
    pub lane: Option<Lane>,
    pub offset: Option<u32>,
    pub unpack: Option<Unpack>,
    pub pos: Pos,
}

/// Why a member cannot be placed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutErrorKind {
    /// `layout(lane = L)` that leaves too few lanes for the member.
    LaneOverflow { lane: Lane, lanes: usize },
    /// `layout(lane = L)` on a member that takes whole qwords.
    LaneOnQwords,
    /// `layout(offset = Q)` before where the members so far end.
    OffsetBackwards { offset: u32, end: u32 },
    /// A member after an open array, or after a struct that ends in one.
    AfterOpen,
    /// An array whose elements have no fixed size (they hold an array of
    /// run-time length) or are open.
    VariableElements,
    /// A type that cannot be in memory (`bool`, `clipflags`, `ref<T>`).
    NotInMemory,
    /// An unpack format whose component count or width does not fit the
    /// member.
    UnpackMismatch { components: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutError {
    pub kind: LayoutErrorKind,
    pub member: String,
    pub pos: Pos,
}

/// What a type takes in memory.
enum Footprint {
    /// Lanes of the current qword.
    Lanes(usize),
    /// Whole qwords, possibly run-time ones, possibly ending open.
    Qwords(Extent, Option<OpenTail>),
}

/// The qwords one element of an array takes: fixed and not open.
fn element_qwords(element: &Type, structs: &[StructDef]) -> Option<u32> {
    match footprint(element, structs)? {
        Footprint::Lanes(_) => Some(1),
        Footprint::Qwords(extent, None) if extent.is_fixed() => Some(extent.fixed),
        Footprint::Qwords(..) => None,
    }
}

fn footprint(ty: &Type, structs: &[StructDef]) -> Option<Footprint> {
    Some(match ty {
        Type::Float | Type::Int16 | Type::Int32 | Type::Uint32 => Footprint::Lanes(1),
        Type::Vec(n) | Type::IVec(n) | Type::UVec(n) => Footprint::Lanes(usize::from(*n)),
        Type::Qword => Footprint::Qwords(Extent::fixed(1), None),
        Type::Mat { size, .. } => Footprint::Qwords(Extent::fixed(u32::from(*size)), None),
        Type::Struct(id) => {
            let def: &StructDef = structs.get(*id)?;
            Footprint::Qwords(def.size.clone(), def.open.clone())
        }
        Type::Array(element, length) => {
            let stride = element_qwords(element, structs)?;
            match length {
                Length::Fixed(n) => Footprint::Qwords(Extent::fixed(n * stride), None),
                Length::Member(count) => Footprint::Qwords(
                    Extent {
                        fixed: 0,
                        terms: vec![Term {
                            count: count.clone(),
                            stride,
                        }],
                    },
                    None,
                ),
                Length::Xitop => Footprint::Qwords(
                    Extent {
                        fixed: 0,
                        terms: vec![Term {
                            count: Vec::new(),
                            stride,
                        }],
                    },
                    None,
                ),
                Length::Open => Footprint::Qwords(
                    Extent::default(),
                    Some(OpenTail {
                        path: Vec::new(),
                        stride,
                    }),
                ),
            }
        }
        Type::Void | Type::Bool | Type::ClipFlags | Type::Ref(_) => return None,
    })
}

/// Places a struct's members one by one.
#[derive(Default)]
pub struct Packer {
    qword: Extent,
    lane: usize,
    members: Vec<MemberDef>,
    open: Option<OpenTail>,
    bounds: Vec<Bound>,
}

impl Packer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The members placed so far.
    pub fn members(&self) -> &[MemberDef] {
        &self.members
    }

    fn next_qword(&mut self) {
        if self.lane > 0 {
            self.qword.fixed += 1;
            self.lane = 0;
        }
    }

    pub fn place(&mut self, spec: MemberSpec, structs: &[StructDef]) -> Result<(), LayoutError> {
        let error = |kind| LayoutError {
            kind,
            member: spec.name.clone(),
            pos: spec.pos,
        };
        if self.open.is_some() {
            return Err(error(LayoutErrorKind::AfterOpen));
        }
        let Some(footprint) = footprint(&spec.ty, structs) else {
            return Err(error(match spec.ty {
                Type::Array(..) => LayoutErrorKind::VariableElements,
                _ => LayoutErrorKind::NotInMemory,
            }));
        };
        if let Some(unpack) = spec.unpack {
            let lanes = match &footprint {
                Footprint::Lanes(lanes) => *lanes,
                Footprint::Qwords(..) => 4,
            };
            // Narrow formats unpack integers: a float member reads 32 bits.
            let fits = usize::from(unpack.components) == lanes
                && (unpack.bits == 32 || !spec.ty.is_float());
            if !fits {
                return Err(error(LayoutErrorKind::UnpackMismatch { components: lanes }));
            }
        }
        if let Some(offset) = spec.offset {
            let end = self.qword.fixed + u32::from(self.lane > 0);
            if offset < self.qword.fixed || (offset == self.qword.fixed && self.lane > 0) {
                return Err(error(LayoutErrorKind::OffsetBackwards { offset, end }));
            }
            if !self.qword.is_fixed() {
                // The arrays before end by the offset, whatever their
                // lengths: a bound on them.
                let mut before = self.qword.clone();
                before.fixed = end;
                self.bounds.push(Bound {
                    end: before,
                    at: offset,
                });
            }
            self.qword = Extent::fixed(offset);
            self.lane = 0;
        }
        let index = self.members.len();
        match footprint {
            Footprint::Lanes(lanes) => {
                let mut first = spec.lane.map_or(self.lane, Lane::index);
                if first + lanes > 4 {
                    if spec.lane.is_some() {
                        return Err(error(LayoutErrorKind::LaneOverflow {
                            lane: Lane::from_index(first).unwrap_or(Lane::X),
                            lanes,
                        }));
                    }
                    first = 0;
                    self.next_qword();
                } else if first < self.lane {
                    // The lane asked for is behind the cursor: next qword.
                    self.next_qword();
                }
                self.members.push(MemberDef {
                    name: spec.name.clone(),
                    ty: spec.ty.clone(),
                    offset: self.qword.clone(),
                    lane: Lane::from_index(first).unwrap_or(Lane::X),
                    unpack: spec.unpack,
                    pos: spec.pos,
                });
                self.lane = first + lanes;
                if self.lane == 4 {
                    self.qword.fixed += 1;
                    self.lane = 0;
                }
            }
            Footprint::Qwords(extent, open) => {
                if spec.lane.is_some() {
                    return Err(error(LayoutErrorKind::LaneOnQwords));
                }
                self.next_qword();
                self.members.push(MemberDef {
                    name: spec.name.clone(),
                    ty: spec.ty.clone(),
                    offset: self.qword.clone(),
                    lane: Lane::X,
                    unpack: spec.unpack,
                    pos: spec.pos,
                });
                // A nested struct's lengths are read through this member,
                // and its bounds hold where it is.
                if let Type::Struct(id) = spec.ty {
                    if self.qword.is_fixed() {
                        let start = self.qword.fixed;
                        for bound in &structs[id].bounds {
                            let mut end = bound.end.nested(index);
                            end.fixed += start;
                            self.bounds.push(Bound {
                                end,
                                at: start + bound.at,
                            });
                        }
                    }
                    self.qword.add(&extent.nested(index));
                } else {
                    self.qword.add(&extent);
                }
                self.open = open.map(|tail| OpenTail {
                    path: std::iter::once(index).chain(tail.path).collect(),
                    stride: tail.stride,
                });
            }
        }
        Ok(())
    }

    /// The members, the size, the open array if any, and the bounds.
    pub fn finish(mut self) -> (Vec<MemberDef>, Extent, Option<OpenTail>, Vec<Bound>) {
        self.next_qword();
        (self.members, self.qword, self.open, self.bounds)
    }
}

/// The struct a type is, if it is one.
pub fn struct_of(ty: &Type) -> Option<StructId> {
    match ty {
        Type::Struct(id) => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, ty: Type) -> MemberSpec {
        MemberSpec {
            name: name.into(),
            ty,
            lane: None,
            offset: None,
            unpack: None,
            pos: Pos::default(),
        }
    }

    fn lay_out(specs: Vec<MemberSpec>, structs: &[StructDef]) -> StructDef {
        let mut packer = Packer::new();
        for spec in specs {
            packer.place(spec, structs).unwrap();
        }
        let (members, size, open, bounds) = packer.finish();
        StructDef {
            name: "S".into(),
            pos: Pos::default(),
            members,
            size,
            open,
            bounds,
        }
    }

    fn placed(def: &StructDef) -> Vec<(u32, char)> {
        def.members
            .iter()
            .map(|member| (member.offset.fixed, member.lane.letter()))
            .collect()
    }

    #[test]
    fn scalars_and_vectors_pack_into_lanes() {
        // Frame: a matrix, two vec4s, vec3 + float in one qword, a vec4.
        let frame = lay_out(
            vec![
                spec(
                    "mvp",
                    Type::Mat {
                        size: 4,
                        row_major: true,
                    },
                ),
                spec("gsScale", Type::Vec(4)),
                spec("gsOffset", Type::Vec(4)),
                spec("clipScale", Type::Vec(3)),
                spec("warpPhase", Type::Float),
                spec("colorClamp", Type::Vec(4)),
            ],
            &[],
        );
        assert_eq!(
            placed(&frame),
            [(0, 'x'), (4, 'x'), (5, 'x'), (6, 'x'), (6, 'w'), (7, 'x')]
        );
        assert_eq!(frame.size, Extent::fixed(8));
        // A vec3 after a float moves on; a lane asked for sticks.
        let mut pos = spec("pos", Type::Vec(3));
        pos.lane = Some(Lane::Y);
        let particle = lay_out(vec![spec("color", Type::Uint32), pos], &[]);
        assert_eq!(placed(&particle), [(0, 'x'), (0, 'y')]);
        let moved = lay_out(vec![spec("a", Type::Vec(2)), spec("b", Type::Vec(3))], &[]);
        assert_eq!(placed(&moved), [(0, 'x'), (1, 'x')]);
        assert_eq!(moved.size, Extent::fixed(2));
    }

    #[test]
    fn dependent_and_open_arrays() {
        let particle = lay_out(vec![spec("p", Type::Vec(4))], &[]);
        let sprite = lay_out(vec![spec("q", Type::Qword); 1], &[]);
        let structs = vec![particle, sprite];
        let mut count = spec("count", Type::Int16);
        count.lane = Some(Lane::W);
        let packet = lay_out(
            vec![
                spec("tags", Type::Array(Box::new(Type::Qword), Length::Fixed(7))),
                spec(
                    "sprites",
                    Type::Array(Box::new(Type::Struct(1)), Length::Open),
                ),
            ],
            &structs,
        );
        let structs = vec![structs[0].clone(), structs[1].clone(), packet];
        let batch = lay_out(
            vec![
                count,
                spec("uv", Type::Vec(4)),
                spec(
                    "particles",
                    Type::Array(Box::new(Type::Struct(0)), Length::Member(vec![0])),
                ),
                spec("packet", Type::Struct(2)),
            ],
            &structs,
        );
        assert_eq!(placed(&batch), [(0, 'w'), (1, 'x'), (2, 'x'), (2, 'x')]);
        let term = Term {
            count: vec![0],
            stride: 1,
        };
        assert_eq!(batch.members[3].offset.terms, std::slice::from_ref(&term));
        assert_eq!(
            batch.size,
            Extent {
                fixed: 9,
                terms: vec![term]
            }
        );
        assert_eq!(
            batch.open,
            Some(OpenTail {
                path: vec![3, 1],
                stride: 1
            })
        );
    }

    #[test]
    fn misplaced_members_are_errors() {
        let mut packer = Packer::new();
        let mut wide = spec("wide", Type::Vec(3));
        wide.lane = Some(Lane::Z);
        assert_eq!(
            packer.place(wide, &[]).unwrap_err().kind,
            LayoutErrorKind::LaneOverflow {
                lane: Lane::Z,
                lanes: 3
            }
        );
        let mut packer = Packer::new();
        packer
            .place(
                spec("open", Type::Array(Box::new(Type::Qword), Length::Open)),
                &[],
            )
            .unwrap();
        assert_eq!(
            packer
                .place(spec("after", Type::Float), &[])
                .unwrap_err()
                .kind,
            LayoutErrorKind::AfterOpen
        );
        let mut packer = Packer::new();
        let mut narrow = spec("pos", Type::Vec(4));
        narrow.unpack = Unpack::from_name("v4_16");
        assert_eq!(
            packer.place(narrow, &[]).unwrap_err().kind,
            LayoutErrorKind::UnpackMismatch { components: 4 }
        );
        let mut packer = Packer::new();
        assert_eq!(
            packer
                .place(spec("flag", Type::Bool), &[])
                .unwrap_err()
                .kind,
            LayoutErrorKind::NotInMemory
        );
        let mut packer = Packer::new();
        packer.place(spec("a", Type::Vec(4)), &[]).unwrap();
        let mut back = spec("b", Type::Float);
        back.offset = Some(0);
        assert_eq!(
            packer.place(back, &[]).unwrap_err().kind,
            LayoutErrorKind::OffsetBackwards { offset: 0, end: 1 }
        );
    }
}
