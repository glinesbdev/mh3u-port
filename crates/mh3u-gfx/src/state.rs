//! What the game has told the GPU so far: the state of one `GX2` context (shaders, buffers, textures, render state). Read off the
//! arguments of the `GX2Set*` calls; nothing is drawn from here.

// the fields are read by the draw path (being written next); until then the state is only recorded
#![allow(dead_code)]

use mh3u_rt::{Ctx, Guest};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Blend {
    pub colour_src: u32,
    pub colour_dst: u32,
    pub colour_op: u32,
    pub separate_alpha: bool,
    pub alpha_src: u32,
    pub alpha_dst: u32,
    pub alpha_op: u32,
}

#[derive(Debug, Clone, Default)]
pub struct AttribBuffer {
    pub address: u32,
    pub size: u32,
    pub stride: u32,
}

#[derive(Debug, Clone, Default)]
pub struct UniformBlock {
    pub address: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub vertex_shader: u32,
    pub pixel_shader: u32,
    pub fetch_shader: u32,
    pub geometry_shader: u32,
    pub attrib_buffers: [AttribBuffer; 16],
    pub vertex_blocks: [UniformBlock; 16],
    pub pixel_blocks: [UniformBlock; 16],
    pub geometry_blocks: [UniformBlock; 16],
    /// The texture and sampler (guest addresses of the `GX2Texture` and `GX2Sampler`) bound to each pixel unit.
    pub pixel_textures: [u32; 18],
    pub pixel_samplers: [u32; 18],
    pub colour_buffers: [u32; 8],
    pub depth_buffer: u32,
    pub viewport: [f32; 6],
    pub scissor: [u32; 4],
    pub blend: [Blend; 8],
    pub target_masks: u32,
    pub colour_control: [u32; 4],
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: u32,
    pub stencil_test: bool,
    pub cull_front: bool,
    pub cull_back: bool,
    pub front_face: u32,
    pub alpha_test: (bool, u32, f32),
    /// Whether clip-space depth is 0 to 1 (set by `GX2SetRasterizerClipControlEx`).
    /// The index that ends a strip (set by `GX2SetPrimitiveRestartIndex`).
    pub restart_index: Option<u32>,
    pub half_z: bool,
    /// `GX2SetShaderModeEx`: 0 uniform registers, 1 uniform blocks, 2 geometry shader, 3 compute.
    pub shader_mode: u32,
}

/// The argument `n` (from 0, counting from `r3`) of a call: in a register, or on the stack for the ninth on. `replay` is the stack of a
/// recorded call being played back.
pub fn arg(c: &Ctx, g: &Guest, replay: Option<&[u32]>, n: usize) -> u32 {
    if n < 8 {
        return c.r[3 + n];
    }
    match replay {
        Some(stack) => stack.get(n - 8).copied().unwrap_or(0),
        // the parameter area starts eight bytes above the stack pointer
        None => g.u32(c.r[1].wrapping_add(8 + 4 * (n as u32 - 8))),
    }
}

/// The floating-point argument `n` (from 0, counting from `f1`).
pub fn farg(c: &Ctx, n: usize) -> f32 {
    f64::from_bits(c.f[1 + n]) as f32
}
