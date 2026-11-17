//! VUSL's types, the VF lanes a value occupies, and how a vector's
//! components are placed in them.

use std::fmt;

/// A lane of a VF register or of a qword in memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Lane {
    X,
    Y,
    Z,
    W,
}

impl Lane {
    pub const ALL: [Lane; 4] = [Lane::X, Lane::Y, Lane::Z, Lane::W];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Option<Lane> {
        Self::ALL.get(index).copied()
    }

    pub fn from_letter(letter: char) -> Option<Lane> {
        match letter {
            'x' => Some(Lane::X),
            'y' => Some(Lane::Y),
            'z' => Some(Lane::Z),
            'w' => Some(Lane::W),
            _ => None,
        }
    }

    pub fn letter(self) -> char {
        ['x', 'y', 'z', 'w'][self.index()]
    }
}

/// The lanes that hold a VF value's components, component by component:
/// `vec3` from a member at `.yzw` is placed in y, z and w, and its `.zy`
/// swizzle in w and z. Values combine element by element only when their
/// placements are the same.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct Placement {
    lanes: [Lane; 4],
    len: u8,
}

impl Placement {
    /// Components in consecutive lanes from `first`: `None` when they would
    /// run past `w`.
    pub fn starting_at(first: Lane, len: usize) -> Option<Self> {
        if len == 0 || first.index() + len > 4 {
            return None;
        }
        let mut lanes = [Lane::X; 4];
        for (component, lane) in lanes.iter_mut().enumerate().take(len) {
            *lane = Lane::from_index(first.index() + component)?;
        }
        Some(Self {
            lanes,
            len: len as u8,
        })
    }

    /// x, xy, xyz or xyzw.
    pub fn identity(len: usize) -> Self {
        Self::starting_at(Lane::X, len).expect("one to four components")
    }

    /// The placement of the given lanes, in component order. `None` when a
    /// lane repeats or there are none or more than four.
    pub fn from_lanes(lanes: &[Lane]) -> Option<Self> {
        if lanes.is_empty() || lanes.len() > 4 {
            return None;
        }
        let mut seen = [false; 4];
        let mut placed = [Lane::X; 4];
        for (component, &lane) in lanes.iter().enumerate() {
            if std::mem::replace(&mut seen[lane.index()], true) {
                return None;
            }
            placed[component] = lane;
        }
        Some(Self {
            lanes: placed,
            len: lanes.len() as u8,
        })
    }

    pub fn lanes(&self) -> &[Lane] {
        &self.lanes[..usize::from(self.len)]
    }

    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The lane of component `component`.
    pub fn lane(&self, component: usize) -> Lane {
        self.lanes()[component]
    }

    /// The placement of some of the components, in the order given.
    pub fn select(&self, components: &[usize]) -> Option<Self> {
        let lanes: Vec<Lane> = components
            .iter()
            .map(|&component| self.lanes().get(component).copied())
            .collect::<Option<_>>()?;
        Self::from_lanes(&lanes)
    }

    /// Whether the lanes rotate onto `other`'s by the same number of lanes
    /// for every component, as MR32 moves them (one lane down a step).
    pub fn rotation_to(&self, other: &Placement) -> Option<usize> {
        if self.len != other.len {
            return None;
        }
        let steps = (self.lane(0).index() + 4 - other.lane(0).index()) % 4;
        self.lanes()
            .iter()
            .zip(other.lanes())
            .all(|(from, to)| (from.index() + 4 - to.index()) % 4 == steps)
            .then_some(steps)
    }

    /// The lanes as letters in component order: `yzw`, `zy`.
    pub fn letters(&self) -> String {
        self.lanes().iter().map(|lane| lane.letter()).collect()
    }
}

impl fmt::Debug for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.letters())
    }
}

/// An index into the program's structs.
pub type StructId = usize;

/// An array's length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Length {
    Fixed(u32),
    /// Read from an `int16` member before it, by its path from the struct
    /// that holds the array (`count`, or `packet.n` once nested).
    Member(Vec<usize>),
    /// `[]`: as many as the program writes.
    Open,
}

/// A VUSL type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    Void,
    Bool,
    /// A VI register's 16 bits; in memory the low half of a lane.
    Int16,
    Float,
    /// `vec2` to `vec4`.
    Vec(u8),
    /// A lane of raw 32-bit integer bits.
    Int32,
    Uint32,
    /// `ivec2` to `ivec4` and `uvec2` to `uvec4`: lanes of raw bits.
    IVec(u8),
    UVec(u8),
    /// 128 opaque bits.
    Qword,
    /// `mat3` or `mat4`: three or four qwords, columns unless `row_major`.
    Mat {
        size: u8,
        row_major: bool,
    },
    /// One CLIPW judgement.
    ClipFlags,
    Struct(StructId),
    Array(Box<Type>, Length),
    /// A qword address in a VI register.
    Ref(Box<Type>),
}

impl Type {
    /// How many lanes a value of the type occupies in a VF register, for
    /// the types that live in one.
    pub fn vf_lanes(&self) -> Option<usize> {
        match self {
            Type::Float | Type::Int32 | Type::Uint32 => Some(1),
            Type::Vec(n) | Type::IVec(n) | Type::UVec(n) => Some(usize::from(*n)),
            Type::Qword => Some(4),
            _ => None,
        }
    }

    /// Floating-point values: FMAC operations apply.
    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float | Type::Vec(_))
    }

    /// Raw integer bits in VF lanes, which only move and convert.
    pub fn is_raw(&self) -> bool {
        matches!(
            self,
            Type::Int32 | Type::Uint32 | Type::IVec(_) | Type::UVec(_) | Type::Qword
        )
    }

    /// A vector of `n` components with this type's elements: `vec2` for a
    /// `float` with 2, `int32` for an `ivec4` with 1.
    pub fn with_components(&self, n: usize) -> Option<Type> {
        let n8 = u8::try_from(n).ok()?;
        match (self, n) {
            (Type::Float | Type::Vec(_), 1) => Some(Type::Float),
            (Type::Float | Type::Vec(_), 2..=4) => Some(Type::Vec(n8)),
            (Type::Int32 | Type::IVec(_), 1) => Some(Type::Int32),
            (Type::Int32 | Type::IVec(_), 2..=4) => Some(Type::IVec(n8)),
            (Type::Uint32 | Type::UVec(_) | Type::Qword, 1) => Some(Type::Uint32),
            (Type::Uint32 | Type::UVec(_) | Type::Qword, 2..=4) => Some(Type::UVec(n8)),
            _ => None,
        }
    }

    /// The scalar type of the components of a VF value.
    pub fn component(&self) -> Option<Type> {
        self.with_components(1)
    }

    /// The name as written in VUSL, given the program's struct names.
    pub fn name(&self, structs: &[String]) -> String {
        match self {
            Type::Void => "void".into(),
            Type::Bool => "bool".into(),
            Type::Int16 => "int16".into(),
            Type::Float => "float".into(),
            Type::Vec(n) => format!("vec{n}"),
            Type::Int32 => "int32".into(),
            Type::Uint32 => "uint32".into(),
            Type::IVec(n) => format!("ivec{n}"),
            Type::UVec(n) => format!("uvec{n}"),
            Type::Qword => "qword".into(),
            Type::Mat { size, row_major } => {
                if *row_major {
                    format!("mat{size} (row_major)")
                } else {
                    format!("mat{size}")
                }
            }
            Type::ClipFlags => "clipflags".into(),
            Type::Struct(id) => structs.get(*id).cloned().unwrap_or_default(),
            Type::Array(element, length) => {
                let length = match length {
                    Length::Fixed(n) => n.to_string(),
                    Length::Member(_) => "...".into(),
                    Length::Open => String::new(),
                };
                format!("{}[{length}]", element.name(structs))
            }
            Type::Ref(target) => format!("ref<{}>", target.name(structs)),
        }
    }
}

/// The built-in types by name; `int` is not one (`int16` is).
pub fn builtin_type(name: &str) -> Option<Type> {
    Some(match name {
        "void" => Type::Void,
        "bool" => Type::Bool,
        "int16" => Type::Int16,
        "float" => Type::Float,
        "vec2" => Type::Vec(2),
        "vec3" => Type::Vec(3),
        "vec4" => Type::Vec(4),
        "int32" => Type::Int32,
        "uint32" => Type::Uint32,
        "ivec2" => Type::IVec(2),
        "ivec3" => Type::IVec(3),
        "ivec4" => Type::IVec(4),
        "uvec2" => Type::UVec(2),
        "uvec3" => Type::UVec(3),
        "uvec4" => Type::UVec(4),
        "qword" => Type::Qword,
        "mat3" => Type::Mat {
            size: 3,
            row_major: false,
        },
        "mat4" => Type::Mat {
            size: 4,
            row_major: false,
        },
        "clipflags" => Type::ClipFlags,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_follow_components() {
        let yzw = Placement::starting_at(Lane::Y, 3).unwrap();
        assert_eq!(yzw.letters(), "yzw");
        assert_eq!(yzw.select(&[2, 1]).unwrap().letters(), "wz");
        assert!(Placement::starting_at(Lane::Z, 3).is_none());
        assert!(Placement::from_lanes(&[Lane::X, Lane::X]).is_none());
        // yzw onto xyz is one MR32 step; xy onto zw two; zy onto xy none.
        let xyz = Placement::identity(3);
        assert_eq!(yzw.rotation_to(&xyz), Some(1));
        let zw = Placement::starting_at(Lane::Z, 2).unwrap();
        assert_eq!(Placement::identity(2).rotation_to(&zw), Some(2));
        let zy = Placement::from_lanes(&[Lane::Z, Lane::Y]).unwrap();
        assert_eq!(zy.rotation_to(&Placement::identity(2)), None);
    }

    #[test]
    fn types_have_their_names() {
        assert_eq!(builtin_type("vec3"), Some(Type::Vec(3)));
        assert_eq!(builtin_type("int"), None);
        assert_eq!(Type::IVec(4).component(), Some(Type::Int32));
        assert_eq!(Type::Float.with_components(3), Some(Type::Vec(3)));
        assert_eq!(
            Type::Array(Box::new(Type::Qword), Length::Fixed(7)).name(&[]),
            "qword[7]"
        );
    }
}
