//! Representative VU1 data memory for the corpus renderers (docs/TESTING.md,
//! "Scenes"): the starting state their hosts leave before MSCAL, built from
//! the layouts those hosts document, with simple geometry whose result is
//! known. Each scene gives the triangles its program must draw, which a
//! reference clipper computes for the scenes that clip.
//!
//! doom3-bfg-ps2 (commit adc427d): `vu_memory.h` maps VU1 memory, the cooked
//! batch packet is described in `src/tools/assets/world.h`, and the frame
//! constants come from `MakeFrameConstants` and `math::PerspectiveProjection`.
//!
//! quake2-ps2 (commit 12f2f9a): `vu1.h` maps VU1 memory and gives the
//! DrawVertex, keyframe and particle batch layouts, and the frame constants
//! come from `render_system.cpp` and the same `math::PerspectiveProjection`.

use crate::vu_model::State;

/// A starting state and the triangles the program must draw from it.
pub struct Scene {
    pub name: String,
    pub state: State,
    /// Primitives drawn, as [`drawn_primitives`] counts them.
    pub primitives: usize,
}

/// The scenes for a corpus case, none for a program without them.
pub fn scenes(case: &str) -> Vec<Scene> {
    match case {
        "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips" => doom3::scenes(false),
        "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_lm" => doom3::scenes(true),
        "quake2-ps2__src_ps2_renderer_vu1progs_textured_triangles" => quake2::triangle_scenes(),
        "quake2-ps2__src_ps2_renderer_vu1progs_particles" => quake2::particle_scenes(),
        _ => Vec::new(),
    }
}

/// Primitives the GS draws from `kicks`. In each PACKED packet with a
/// primitive (PRE set), every XYZ2 write is a vertex kick; a triangle list
/// closes a triangle every third kick, a sprite every second, and a strip
/// or fan every kick. A primitive is drawn when its closing kick has ADC
/// clear.
pub fn drawn_primitives(kicks: &[(u16, Vec<[u32; 4]>)]) -> usize {
    let mut drawn = 0;
    for (_, packet) in kicks {
        let mut index = 0;
        while index < packet.len() {
            let tag = packet[index];
            index += 1;
            let loops = (tag[0] & 0x7fff) as usize;
            let flg = (tag[1] >> 26) & 3;
            let registers = match tag[1] >> 28 {
                0 => 16,
                count => count as usize,
            };
            let size = match flg {
                0 => loops * registers,
                1 => (loops * registers).div_ceil(2),
                _ => loops,
            };
            let prim = (tag[1] & (1 << 14) != 0).then_some((tag[1] >> 15) & 7);
            if let (0, Some(prim)) = (flg, prim) {
                let regs = u64::from(tag[2]) | (u64::from(tag[3]) << 32);
                let kicks_at: Vec<usize> = (0..registers)
                    .filter(|&r| (regs >> (4 * r)) & 0xf == 0x5)
                    .collect();
                let every = match prim {
                    3 => 3,
                    6 => 2,
                    _ => 1,
                };
                let mut kick = 0;
                for vertex in 0..loops {
                    for &register in &kicks_at {
                        kick += 1;
                        let at = index + vertex * registers + register;
                        let xyz = packet.get(at).copied().unwrap_or([0; 4]);
                        if kick % every == 0 && xyz[3] & 0x8000 == 0 {
                            drawn += 1;
                        }
                    }
                }
            }
            index += size;
            if tag[0] & 0x8000 != 0 {
                break;
            }
        }
    }
    drawn
}

/// Drawn vertices whose XYZ2 lies outside the GS's 4096x4096 coordinate
/// space (12.4 fixed point, so 0 to 0xffff).
pub fn off_window(kicks: &[(u16, Vec<[u32; 4]>)]) -> usize {
    let mut off = 0;
    for (_, packet) in kicks {
        let mut index = 0;
        while index < packet.len() {
            let tag = packet[index];
            index += 1;
            let loops = (tag[0] & 0x7fff) as usize;
            let flg = (tag[1] >> 26) & 3;
            let registers = match tag[1] >> 28 {
                0 => 16,
                count => count as usize,
            };
            let size = match flg {
                0 => loops * registers,
                1 => (loops * registers).div_ceil(2),
                _ => loops,
            };
            if flg == 0 && tag[1] & (1 << 14) != 0 {
                let regs = u64::from(tag[2]) | (u64::from(tag[3]) << 32);
                for vertex in 0..loops {
                    for register in (0..registers).filter(|&r| (regs >> (4 * r)) & 0xf == 0x5) {
                        let xyz = packet
                            .get(index + vertex * registers + register)
                            .copied()
                            .unwrap_or([0; 4]);
                        if xyz[3] & 0x8000 == 0 && (xyz[0] > 0xffff || xyz[1] > 0xffff) {
                            off += 1;
                        }
                    }
                }
            }
            index += size;
            if tag[0] & 0x8000 != 0 {
                break;
            }
        }
    }
    off
}

fn bits(value: f32) -> u32 {
    value.to_bits()
}

/// Row-vector 4x4 matrix product `a * b`.
fn multiply(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| (0..4).map(|k| a[row][k] * b[k][column]).sum())
    })
}

/// A GIF tag qword: NLOOP, EOP, PRE with PRIM, FLG, NREG and REGS.
fn gif_tag(loops: u32, eop: bool, prim: Option<u32>, flg: u32, nreg: u32, regs: u32) -> [u32; 4] {
    let mut low = loops & 0x7fff;
    if eop {
        low |= 0x8000;
    }
    let mut high = (flg << 26) | (nreg << 28);
    if let Some(prim) = prim {
        // PRE is bit 46 and PRIM bits 47-57 of the tag.
        high |= 1 << 14;
        high |= (prim & 0x7ff) << 15;
    }
    [low, high, regs, 0]
}

// The constants both ports share (doom3's vu.h and gs.cpp, quake2's vu1.h).
const GUARD_BAND: f32 = 1.0 / 0.8;
const CLIP_SHRINK: f32 = 0.001;
const CLIP_DISTANCE_SCALE: f32 = 2048.0 * 2048.0;
const DEPTH_SCALE: f32 = 65535.0 / 32.0;
const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 448.0;

/// A camera: the eye and the yaw and pitch it looks along.
struct Camera {
    eye: [f32; 3],
    /// The near and far planes.
    near: f32,
    far: f32,
    /// Radians about the vertical axis, 0 looking along -z.
    yaw: f32,
    /// Radians, positive looking up.
    pitch: f32,
}

impl Camera {
    /// `math::PerspectiveProjection`, which both ports share, for a 90
    /// degree horizontal field of view on a 640x448 screen, after a view
    /// matrix with x right, y up and z behind the eye.
    fn view_projection(&self) -> [[f32; 4]; 4] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let forward = [-sy * cp, sp, -cy * cp];
        let back = forward.map(|v| -v);
        let right = [cy, 0.0, -sy];
        let up = cross(back, right);
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let e = self.eye;
        let view = [
            [right[0], up[0], back[0], 0.0],
            [right[1], up[1], back[1], 0.0],
            [right[2], up[2], back[2], 0.0],
            [-dot(right, e), -dot(up, e), -dot(back, e), 1.0],
        ];
        let tan_y = HEIGHT / WIDTH;
        let cot = 1.0 / tan_y;
        let aspect = 1.0 / tan_y;
        let w = cot * (WIDTH / 4096.0) / aspect;
        let h = cot * (HEIGHT / 4096.0);
        let projection = [
            [w, 0.0, 0.0, 0.0],
            [0.0, -h, 0.0, 0.0],
            [
                0.0,
                0.0,
                (self.far + self.near) / (self.far - self.near),
                -1.0,
            ],
            [
                0.0,
                0.0,
                2.0 * self.far * self.near / (self.far - self.near),
                0.0,
            ],
        ];
        multiply(&view, &projection)
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Triangles one triangle becomes: one when it is inside the guard band
/// (clipw), else the fan of its Sutherland-Hodgman polygon against the
/// near plane and the four guard band sides, as both ports' `vu_clip.i` cut them:
/// d = w (1 - shrink) + the plane's term, a corner kept when d >= 0.
fn reference_clip(corners: Vec<[f64; 4]>) -> usize {
    let g = f64::from(GUARD_BAND);
    let inside = corners
        .iter()
        .all(|c| (c[0] * g).abs() <= c[3] && (c[1] * g).abs() <= c[3] && c[2].abs() <= c[3]);
    if inside {
        return 1;
    }
    let shrink = 1.0 - f64::from(CLIP_SHRINK);
    let planes: [fn(&[f64; 4], f64) -> f64; 5] = [
        |c, _| -c[2],
        |c, g| -c[0] * g,
        |c, g| c[0] * g,
        |c, g| -c[1] * g,
        |c, g| c[1] * g,
    ];
    let mut polygon = corners;
    for plane in planes {
        let distance = |c: &[f64; 4]| c[3] * shrink + plane(c, g);
        // A triangle wholly outside one plane is rejected.
        if polygon.iter().all(|c| distance(c) < 0.0) {
            return 0;
        }
        let mut next = Vec::new();
        for i in 0..polygon.len() {
            let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
            let (da, db) = (distance(&a), distance(&b));
            if da >= 0.0 {
                next.push(a);
            }
            if da * db < 0.0 {
                let t = da / (da - db);
                next.push(std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t));
            }
        }
        polygon = next;
    }
    polygon.len().saturating_sub(2)
}

/// The clip-space corners of model points under `mvp`, in f64.
fn clip_space(mvp: &[[f32; 4]; 4], p: [f32; 3]) -> [f64; 4] {
    std::array::from_fn(|column| {
        (0..3)
            .map(|k| f64::from(p[k]) * f64::from(mvp[k][column]))
            .sum::<f64>()
            + f64::from(mvp[3][column])
    })
}

mod doom3 {
    use super::*;

    // vu_memory.h
    const INPUT_BASE: u16 = 40;
    const WINDOW_A: u32 = 652;
    const WINDOW_QWORDS: u32 = 151;
    const BATCH_LIGHTMAP: usize = 295;
    const BATCH_STATE: usize = 297;
    const BATCH_STRIP_TAG: usize = 304;
    const BATCH_LIST_TAG: usize = 305;
    // vu.h and gs.cpp
    // GS: PRIM triangle (3) or strip (4), with TME; the registers ST,
    // RGBAQ, XYZ2.
    const PRIM_LIST: u32 = 3 | 0x10;
    const PRIM_STRIP: u32 = 4 | 0x10;
    const REGS: u32 = 0x512;

    /// Qwords 0-12: `DisplayList::SetFrameConstants`.
    fn frame(state: &mut State, camera: &Camera) {
        let mvp = camera.view_projection();
        for (row, values) in mvp.iter().enumerate() {
            state.memory[row] = values.map(bits);
        }
        state.memory[4] = [2048.0, 2048.0, DEPTH_SCALE, 1.0 - CLIP_SHRINK].map(bits);
        state.memory[5] = [
            2048.0 + WIDTH / 2.0,
            2048.0 + HEIGHT / 2.0,
            DEPTH_SCALE,
            CLIP_DISTANCE_SCALE,
        ]
        .map(bits);
        state.memory[6] = [GUARD_BAND, GUARD_BAND, 1.0, 0.0].map(bits);
        state.memory[7] = [WINDOW_A, WINDOW_QWORDS, 0, 0];
        state.memory[8] = gif_tag(0, true, None, 0, 0, 0);
        state.memory[9] = gif_tag(0, false, Some(PRIM_STRIP), 0, 3, REGS);
        state.memory[10] = gif_tag(0, false, Some(PRIM_LIST), 0, 3, REGS);
        state.memory[11] = gif_tag(0, false, Some(PRIM_STRIP | 0x40), 0, 3, REGS);
        state.memory[12] = gif_tag(0, false, Some(PRIM_LIST | 0x40), 0, 3, REGS);
    }

    /// A grid of `rows` x `columns` quads from `origin` along `across` and
    /// `down`: its vertices, and its strips, one per row, as vertex indices.
    struct Grid {
        vertices: Vec<[f32; 3]>,
        strips: Vec<Vec<usize>>,
    }

    fn grid(
        origin: [f32; 3],
        across: [f32; 3],
        down: [f32; 3],
        rows: usize,
        columns: usize,
    ) -> Grid {
        let mut vertices = Vec::new();
        for row in 0..=rows {
            for column in 0..=columns {
                vertices.push(std::array::from_fn(|axis| {
                    origin[axis] + across[axis] * column as f32 + down[axis] * row as f32
                }));
            }
        }
        let at = |row: usize, column: usize| row * (columns + 1) + column;
        let strips = (0..rows)
            .map(|row| {
                (0..=columns)
                    .flat_map(|column| [at(row, column), at(row + 1, column)])
                    .collect()
            })
            .collect();
        Grid { vertices, strips }
    }

    /// The batch at `base`, as the cooked packet leaves it: the header, the
    /// index stream (indexed batches), the vertices under STCYCL 4,1 (s16
    /// position, s16 UVs, RGBA bytes), then the batch state the display
    /// list adds. Returns XITOP.
    fn batch(state: &mut State, base: usize, grid: &Grid, indexed: bool, lightmap: bool) -> u16 {
        let lo = (0..3).map(|axis| {
            grid.vertices
                .iter()
                .map(|v| v[axis])
                .fold(f32::MAX, f32::min)
        });
        let hi = (0..3).map(|axis| {
            grid.vertices
                .iter()
                .map(|v| v[axis])
                .fold(f32::MIN, f32::max)
        });
        let center: Vec<f32> = lo.zip(hi).map(|(a, b)| (a + b) / 2.0).collect();
        let extent = grid
            .vertices
            .iter()
            .flat_map(|v| {
                (0..3)
                    .map(|axis| (v[axis] - center[axis]).abs())
                    .collect::<Vec<_>>()
            })
            .fold(1.0, f32::max);
        let scale = extent / 30000.0;
        let (uv_scale, uv_offset) = (1.0 / 256.0, [0.0, 0.0]);
        // Strip order: plain strips repeat shared vertices; indexed strips
        // name unique ones.
        let entries: Vec<(usize, bool)> = grid
            .strips
            .iter()
            .flat_map(|strip| strip.iter().enumerate().map(|(i, &v)| (v, i < 2)))
            .collect();
        let vertices: Vec<usize> = if indexed {
            (0..grid.vertices.len()).collect()
        } else {
            entries.iter().map(|&(v, _)| v).collect()
        };
        let index_qwords = if indexed {
            entries.len().div_ceil(4)
        } else {
            0
        };
        state.memory[base] = [
            bits(center[0]),
            bits(center[1]),
            bits(center[2]),
            index_qwords as u32,
        ];
        state.memory[base + 1] = [scale, uv_scale, uv_offset[0], uv_offset[1]].map(bits);
        let first = 2 + index_qwords;
        if indexed {
            let entry =
                |&(v, adc): &(usize, bool)| (first + 4 * v) as u32 | if adc { 0x8000 } else { 0 };
            let pad = (first as u32) | 0x8000;
            for q in 0..index_qwords {
                state.memory[base + 2 + q] =
                    std::array::from_fn(|lane| entries.get(4 * q + lane).map_or(pad, entry));
            }
        }
        for (slot, &v) in vertices.iter().enumerate() {
            let at = base + first + 4 * slot;
            let p = grid.vertices[v];
            let q = |axis: usize| ((p[axis] - center[axis]) / scale).round() as i32 as u32;
            let adc = !indexed && entries[slot].1;
            state.memory[at] = [q(0), q(1), q(2), if adc { 0xffff_8000 } else { 0 }];
            let (s, t) = ((v % 17) as i32 * 64, (v / 17) as i32 * 64);
            state.memory[at + 1] = [s as u32, t as u32, (s * 16) as u32, (t * 16) as u32];
            state.memory[at + 2] = [0x80, 0x70, 0x60, 0x80];
            state.memory[at + 3] = [0; 4];
        }
        assert!(
            first + 4 * vertices.len() <= BATCH_LIGHTMAP,
            "batch too large"
        );
        // The lightmap page's A+D packet (TEX0's word 3 set for a lit
        // batch), the material's, then the drawing tags.
        state.memory[base + BATCH_LIGHTMAP] = gif_tag(1, true, None, 0, 1, 0xe);
        state.memory[base + BATCH_LIGHTMAP + 1] = [0x100, 0, 0x06, u32::from(lightmap)];
        state.memory[base + BATCH_STATE] = gif_tag(6, true, None, 0, 1, 0xe);
        for (k, register) in [0x06, 0x14, 0x34, 0x36, 0x08, 0x47].into_iter().enumerate() {
            state.memory[base + BATCH_STATE + 1 + k] = [0x1000 + k as u32, 0, register, 0];
        }
        state.memory[base + BATCH_STRIP_TAG] = gif_tag(0, false, Some(PRIM_STRIP), 0, 3, REGS);
        state.memory[base + BATCH_LIST_TAG] = gif_tag(0, false, Some(PRIM_LIST), 0, 3, REGS);
        vertices.len() as u16
    }

    fn state(camera: &Camera, grid: &Grid, indexed: bool, lightmap: bool) -> State {
        let mut state = State::default();
        frame(&mut state, camera);
        state.top = INPUT_BASE;
        state.itop = batch(&mut state, usize::from(INPUT_BASE), grid, indexed, lightmap);
        state
    }

    /// The triangles the program draws: the reference clipper's count for
    /// each strip triangle, once per pass.
    fn expected(camera: &Camera, grid: &Grid, lightmap: bool) -> usize {
        let mvp = camera.view_projection();
        let clip = |p: [f32; 3]| clip_space(&mvp, p);
        let mut triangles = 0;
        for strip in &grid.strips {
            for corner in strip.windows(3) {
                let corners = corner.iter().map(|&v| clip(grid.vertices[v])).collect();
                triangles += reference_clip(corners);
            }
        }
        triangles * if lightmap { 2 } else { 1 }
    }

    pub fn scenes(indexed: bool) -> Vec<Scene> {
        let ahead = Camera {
            eye: [0.0, 48.0, 0.0],
            near: 3.0,
            far: 65536.0,
            yaw: 0.0,
            pitch: 0.0,
        };
        let rows = 2;
        let columns = 16;
        // A wall well ahead: every vertex inside the guard band, so the
        // strip path, which nearly every batch takes.
        let wall = grid(
            [-256.0, 160.0, -400.0],
            [32.0, 0.0, 0.0],
            [0.0, -32.0, 0.0],
            rows,
            columns,
        );
        // The floor under the eye, from behind it to ahead: corners behind
        // the near plane, so the list path and the clipper.
        let floor = grid(
            [-96.0, 0.0, 64.0],
            [12.0, 0.0, 0.0],
            [0.0, 0.0, -40.0],
            rows,
            columns,
        );
        // A wall seen edge-on far to the right: past the guard band's side.
        let side = Camera {
            eye: [0.0, 48.0, 0.0],
            near: 3.0,
            far: 65536.0,
            yaw: 1.3,
            pitch: 0.0,
        };
        let mut scenes = Vec::new();
        for (name, camera, grid) in [
            ("wall", &ahead, &wall),
            ("floor", &ahead, &floor),
            ("side", &side, &wall),
        ] {
            for lightmap in [false, true] {
                if lightmap && !indexed {
                    continue;
                }
                scenes.push(Scene {
                    name: format!("{name}{}", if lightmap { " lit" } else { "" }),
                    state: state(camera, grid, indexed, lightmap),
                    primitives: expected(camera, grid, lightmap),
                });
            }
        }
        scenes
    }
}

mod quake2 {
    use super::*;

    // vu1.h
    const DOUBLE_BUFFER_BASE: u16 = 8;
    const LERP_BLOCK: usize = 1010;
    const CULL_SIGN_SCALE: f32 = 1_099_511_627_776.0; // 2^40, rs::kCullSignScale
    const PARTICLE_BLOW_UP: f32 = 0.004;
    // GS registers: ST 0x2, RGBAQ 0x1, UV 0x3, XYZ2 0x5, A+D 0xe; PRIM
    // triangle 3, sprite 6, with TME and FST off.
    const DRAW_VERTEX_REGS: u32 = 0x2 | (0xe << 4) | (0x5 << 8);
    const COMPUTED_REGS: u32 = 0x2 | (0x1 << 4) | (0x5 << 8);
    const PARTICLE_REGS: u32 = 0xe | (0x3 << 4) | (0x5 << 8) | (0x3 << 12) | (0x5 << 16);

    fn camera(eye: [f32; 3], yaw: f32, pitch: f32) -> Camera {
        // view.cpp: kZNear 4, kZFar 4096.
        Camera {
            eye,
            near: 4.0,
            far: 4096.0,
            yaw,
            pitch,
        }
    }

    /// Qwords 0-7: `render_system.cpp`'s FrameConstants for `mvp`.
    fn frame(state: &mut State, mvp: &[[f32; 4]; 4]) {
        for (row, values) in mvp.iter().enumerate() {
            state.memory[row] = values.map(bits);
        }
        state.memory[4] = [2048.0, 2048.0, DEPTH_SCALE, 1.0 - CLIP_SHRINK].map(bits);
        state.memory[5] = [
            2048.0 + WIDTH / 2.0,
            2048.0 + HEIGHT / 2.0,
            DEPTH_SCALE,
            CLIP_DISTANCE_SCALE,
        ]
        .map(bits);
        state.memory[6] = [GUARD_BAND, GUARD_BAND, 1.0, 0.0].map(bits);
        state.memory[7] = [255.0; 4].map(bits);
    }

    /// The 7 GIF tag qwords at `at`: the A+D set tag, MIPTBP1, TEX1, TEX0,
    /// ALPHA and ZBUF, then the drawing tag with `loops`, which the
    /// triangle program replaces and the particle program keeps.
    fn gif_tags(state: &mut State, at: usize, loops: u32, prim: u32, nreg: u32, regs: u32) {
        state.memory[at] = gif_tag(5, false, None, 0, 1, 0xe);
        for (k, register) in [0x34, 0x14, 0x06, 0x42, 0x4e].into_iter().enumerate() {
            state.memory[at + 1 + k] = [0x2000 + k as u32, 0, register, 0];
        }
        state.memory[at + 6] = gif_tag(loops, true, Some(prim), 0, nreg, regs);
    }

    /// Triangles over a grid of `rows` x `columns` quads, as a triangle list.
    fn grid_triangles(
        origin: [f32; 3],
        across: [f32; 3],
        down: [f32; 3],
        rows: usize,
        columns: usize,
    ) -> Vec<[[f32; 3]; 3]> {
        let at = |row: usize, column: usize| -> [f32; 3] {
            std::array::from_fn(|axis| {
                origin[axis] + across[axis] * column as f32 + down[axis] * row as f32
            })
        };
        let mut triangles = Vec::new();
        for row in 0..rows {
            for column in 0..columns {
                let (a, b, c, d) = (
                    at(row, column),
                    at(row, column + 1),
                    at(row + 1, column),
                    at(row + 1, column + 1),
                );
                triangles.push([a, c, b]);
                triangles.push([b, c, d]);
            }
        }
        triangles
    }

    /// A world batch of DrawVertex triangles with packed colours.
    fn draw_vertex_scene(name: &str, camera: &Camera, triangles: &[[[f32; 3]; 3]]) -> Scene {
        let mvp = camera.view_projection();
        let mut state = State::default();
        frame(&mut state, &mvp);
        let base = usize::from(DOUBLE_BUFFER_BASE);
        let count = triangles.len() * 3;
        assert!(count <= 90, "a DrawVertex batch holds 90 vertices");
        state.memory[base] = [0, 0, 0, count as u32];
        state.memory[base + 1] = [0; 4];
        gif_tags(&mut state, base + 2, 0, 3, 3, DRAW_VERTEX_REGS);
        for (k, p) in triangles.iter().flatten().enumerate() {
            let at = base + 9 + 2 * k;
            state.memory[at] = [bits(p[0]), bits(p[1]), bits(p[2]), 0];
            let rgba = 0x80 | (0x60 << 8) | (0x40 << 16) | (0x80 << 24);
            state.memory[at + 1] = [rgba, bits(k as f32 * 0.25), bits(0.5), 0];
        }
        state.top = DOUBLE_BUFFER_BASE;
        state.itop = 0;
        let primitives = triangles
            .iter()
            .map(|t| reference_clip(t.iter().map(|&p| clip_space(&mvp, p)).collect()))
            .sum();
        Scene {
            name: name.into(),
            state,
            primitives,
        }
    }

    /// The corners of a box, as MD2 frame bytes: 0..=255 per axis.
    const BOX: [[u32; 3]; 8] = [
        [0, 0, 0],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [0, 0, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    /// Its 12 triangles, two per side.
    const BOX_FACES: [[usize; 3]; 12] = [
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];

    /// The box's triangles, every one wound the same way seen from outside,
    /// as a closed model's are, and the way the cull sign below keeps.
    fn box_faces() -> Vec<[usize; 3]> {
        let point = |c: usize| BOX[c].map(|b| b as f32 - 127.5);
        BOX_FACES
            .iter()
            .map(|&[a, b, c]| {
                let (pa, pb, pc) = (point(a), point(b), point(c));
                let e1 = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
                let e2 = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
                let n = cross(e1, e2);
                let outward = n[0] * pa[0] + n[1] * pa[1] + n[2] * pa[2] > 0.0;
                if outward { [a, c, b] } else { [a, b, c] }
            })
            .collect()
    }

    /// A keyframe batch of the box, lerped halfway between two frames, its
    /// colour computed from the entity's light and the shade.
    fn keyframe_scene(name: &str, camera: &Camera, origin: [f32; 3], size: f32) -> Scene {
        let view_projection = camera.view_projection();
        // The frame's scale and translate: bytes * scale + translate.
        let scale = size / 255.0;
        let model = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [origin[0], origin[1], origin[2], 1.0],
        ];
        let mvp = multiply(&model, &view_projection);
        let mut state = State::default();
        frame(&mut state, &mvp);
        let base = usize::from(DOUBLE_BUFFER_BASE);
        let faces = box_faces();
        let count = faces.len() * 3;
        state.memory[base] = [1, 0, 1, count as u32];
        state.memory[base + 1] = [0; 4];
        gif_tags(&mut state, base + 2, 0, 3, 3, COMPUTED_REGS);
        let backlerp = 0.5_f32;
        for (k, &corner) in faces.iter().flatten().enumerate() {
            let at = base + 9 + 3 * k;
            let bytes = BOX[corner];
            state.memory[at] = [bytes[0], bytes[1], bytes[2], 7];
            // The old frame: the same pose, its 4th byte the shade * 128.
            state.memory[at + 1] = [bytes[0], bytes[1], bytes[2], 160];
            state.memory[at + 2] = [0, bits(corner as f32 * 0.125), bits(0.25), bits(1.0)];
        }
        state.memory[LERP_BLOCK] = [
            scale * (1.0 - backlerp),
            scale * (1.0 - backlerp),
            scale * (1.0 - backlerp),
            0.0,
        ]
        .map(bits);
        state.memory[LERP_BLOCK + 1] =
            [scale * backlerp, scale * backlerp, scale * backlerp, 0.0].map(bits);
        state.memory[LERP_BLOCK + 2] = [1.0, 0.9, 0.8, 128.0].map(bits);
        // FaceCull::Negative: culls negative determinants.
        state.memory[LERP_BLOCK + 3] = [CULL_SIGN_SCALE, 1.0 / 64.0, 1.0 / 64.0, 1.0].map(bits);
        state.top = DOUBLE_BUFFER_BASE;
        state.itop = 0;
        let point = |corner: usize| -> [f32; 3] { BOX[corner].map(|byte| byte as f32 * scale) };
        let primitives = faces
            .iter()
            .map(|face| {
                let corners: Vec<[f64; 4]> =
                    face.iter().map(|&c| clip_space(&mvp, point(c))).collect();
                // det[x y w] of the corners: p0 . (e1 x e2).
                let xyw = |c: &[f64; 4]| [c[0], c[1], c[3]];
                let (p0, p1, p2) = (xyw(&corners[0]), xyw(&corners[1]), xyw(&corners[2]));
                let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
                let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
                let normal = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let det = p0[0] * normal[0] + p0[1] * normal[1] + p0[2] * normal[2];
                if det * f64::from(CULL_SIGN_SCALE) <= -1.0 / 16.0 {
                    0
                } else {
                    reference_clip(corners)
                }
            })
            .sum();
        Scene {
            name: name.into(),
            state,
            primitives,
        }
    }

    pub fn triangle_scenes() -> Vec<Scene> {
        let ahead = camera([0.0, 32.0, 0.0], 0.0, 0.0);
        // A wall ahead: 30 triangles, every one inside the guard band.
        let wall = grid_triangles(
            [-240.0, 120.0, -320.0],
            [32.0, 0.0, 0.0],
            [0.0, -40.0, 0.0],
            1,
            15,
        );
        // The floor from behind the eye to ahead: near plane cuts.
        let floor = grid_triangles(
            [-80.0, 0.0, 40.0],
            [16.0, 0.0, 0.0],
            [0.0, 0.0, -48.0],
            3,
            5,
        );
        vec![
            draw_vertex_scene("world wall", &ahead, &wall),
            draw_vertex_scene("world floor", &ahead, &floor),
            // Below and to the right of the eye: three sides face it.
            keyframe_scene("model", &ahead, [40.0, -40.0, -160.0], 48.0),
            // A model beside the eye: its near side runs past the eye,
            // across the near plane.
            keyframe_scene("model near", &ahead, [2.0, 8.0, -30.0], 48.0),
        ]
    }

    /// A burst of 60 particles ahead, a third of them behind the eye or
    /// past the guard band's sides, which the program sends with ADC set.
    pub fn particle_scenes() -> Vec<Scene> {
        let ahead = camera([0.0, 32.0, 0.0], 0.0, 0.0);
        let mvp = ahead.view_projection();
        let mut state = State::default();
        frame(&mut state, &mvp);
        let base = usize::from(DOUBLE_BUFFER_BASE);
        let positions: Vec<[f32; 3]> = (0..60)
            .map(|k| {
                let k = k as f32;
                match k as usize % 3 {
                    0 if (k as usize).is_multiple_of(6) => [k * 4.0 - 120.0, 30.0, 60.0],
                    0 => [4000.0 + k, 30.0, -300.0],
                    _ => [k * 3.0 - 90.0, 20.0 + k, -200.0 - k * 2.0],
                }
            })
            .collect();
        state.memory[base] = [0, 0, 0, positions.len() as u32];
        // (up + right) * 1.5 in clip space for a 1-unit particle, w 0.
        let corner = [
            1.5 * mvp[0][0] + 1.5 * mvp[1][0],
            1.5 * mvp[0][1] + 1.5 * mvp[1][1],
            0.0,
        ];
        state.memory[base + 1] = [corner[0], corner[1], corner[2], PARTICLE_BLOW_UP].map(bits);
        state.memory[base + 2] = [0.0, 0.0, 0.0, 0.0].map(bits);
        state.memory[base + 3] = [16.0, 16.0, 0.0, 0.0].map(bits);
        gif_tags(
            &mut state,
            base + 4,
            positions.len() as u32,
            6,
            5,
            PARTICLE_REGS,
        );
        for (k, p) in positions.iter().enumerate() {
            let rgba = 0xff | (0x80 << 8) | (0x20 << 16) | (0x80 << 24);
            state.memory[base + 11 + k] = [rgba, bits(p[0]), bits(p[1]), bits(p[2])];
        }
        state.top = DOUBLE_BUFFER_BASE;
        let primitives = positions
            .iter()
            .filter(|&&p| {
                let c = clip_space(&mvp, p);
                let g = f64::from(GUARD_BAND);
                (c[0] * g).abs() <= c[3] && (c[1] * g).abs() <= c[3] && c[2].abs() <= c[3]
            })
            .count();
        vec![Scene {
            name: "burst".into(),
            state,
            primitives,
        }]
    }
}
