//! Render pipelines made off the render thread, and remembered on disk.
//!
//! Making a render pipeline compiles its shaders in the driver, which takes from a few to tens of milliseconds; on entering an area the
//! game draws hundreds of new mesh and material combinations, and run 11 spent 3.5 s of one ten-second window in it, stopping the frame
//! each time. So:
//!
//! - **On worker threads** (MH3U_GFX_ASYNC_PIPELINES, on unless set to `0`, `off`, `no` or `false`): a draw whose pipeline is not made
//!   yet hands its description to a worker and is not drawn; the draws of the frames after it are, once the worker is done (usually a
//!   frame or two). With it off, the draw waits for its pipeline as before.
//! - **On disk** (under the run's output folder, `<out>/cache/pipelines/<key>/`; MH3U_GFX_PIPELINE_CACHE=off turns it off,
//!   MH3U_GFX_CACHE_DIR puts it elsewhere): each pipeline made is written down as its two shaders' WGSL (`shaders/<id>.wgsl`) and its
//!   state (a line of `pipelines.txt`), and at the next start all of them are made again on the workers while the game loads, so an
//!   area seen before draws at once. The key is a hash of the build (`BUILD`) and the adapter and driver, so another GPU, driver or build
//!   starts a cache of its own (written down in `about.txt`).
//!
//! What a pipeline is made from is its `PipelineState` (everything of the draw's state that the pipeline holds) and its two shaders. A
//! pipeline is found by the shaders' WGSL (a hash of it) and that state, so one made from the disk cache serves the draw that needs it
//! whatever addresses its shaders have this time.

use crate::draw::Compiled;
use crate::state::Blend;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Everything of a draw's state that its render pipeline holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PipelineState {
    pub topology: wgpu::PrimitiveTopology,
    pub target_format: wgpu::TextureFormat,
    pub blend: Blend,
    pub depth_format: Option<wgpu::TextureFormat>,
    pub blend_on: u32,
    pub masks: u32,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: u32,
    /// Front culled times two plus back culled.
    pub cull: u8,
    pub front_face: u32,
    /// The stencil test when it is on (and the depth buffer has stencil): the faces' settings and the read and write masks.
    pub stencil: Option<([u32; 4], [u32; 4], u32, u32)>,
    /// The depth bias: constant, and the bits of the slope scale and the clamp.
    pub bias: (i32, u32, u32),
}

const TOPOLOGIES: [wgpu::PrimitiveTopology; 5] = [
    wgpu::PrimitiveTopology::PointList,
    wgpu::PrimitiveTopology::LineList,
    wgpu::PrimitiveTopology::LineStrip,
    wgpu::PrimitiveTopology::TriangleList,
    wgpu::PrimitiveTopology::TriangleStrip,
];

/// The formats a game target can have here (see `gx2::texture_format` and `draw::depth_format`), as numbered on disk.
const FORMATS: [wgpu::TextureFormat; 11] = [
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Rgba8UnormSrgb,
    wgpu::TextureFormat::Rgb10a2Unorm,
    wgpu::TextureFormat::Rgba16Float,
    wgpu::TextureFormat::R8Unorm,
    wgpu::TextureFormat::Rg8Unorm,
    wgpu::TextureFormat::R32Float,
    wgpu::TextureFormat::Rg32Float,
    wgpu::TextureFormat::Rgba32Float,
    wgpu::TextureFormat::Depth24PlusStencil8,
    wgpu::TextureFormat::Depth32Float,
];

fn index_of<T: PartialEq>(all: &[T], v: &T) -> Option<u64> {
    all.iter().position(|x| x == v).map(|n| n as u64)
}

impl PipelineState {
    /// The state as numbers, for the disk cache (None for a format the table above does not have: not written down).
    pub(crate) fn encode(&self) -> Option<Vec<u64>> {
        let b = &self.blend;
        let (has_stencil, (f, k, r, w)) = match self.stencil {
            Some(s) => (1, s),
            None => (0, ([0; 4], [0; 4], 0, 0)),
        };
        let mut v = vec![
            index_of(&TOPOLOGIES, &self.topology)?,
            index_of(&FORMATS, &self.target_format)?,
            match self.depth_format {
                Some(d) => index_of(&FORMATS, &d)? + 1,
                None => 0,
            },
            u64::from(self.blend_on),
            u64::from(self.masks),
            u64::from(self.depth_test),
            u64::from(self.depth_write),
            u64::from(self.depth_func),
            u64::from(self.cull),
            u64::from(self.front_face),
            u64::from(b.colour_src),
            u64::from(b.colour_dst),
            u64::from(b.colour_op),
            u64::from(b.separate_alpha),
            u64::from(b.alpha_src),
            u64::from(b.alpha_dst),
            u64::from(b.alpha_op),
            has_stencil,
        ];
        v.extend(f.iter().chain(k.iter()).map(|&x| u64::from(x)));
        v.extend([u64::from(r), u64::from(w)]);
        v.extend([u64::from(self.bias.0 as u32), u64::from(self.bias.1), u64::from(self.bias.2)]);
        Some(v)
    }

    pub(crate) fn decode(v: &[u64]) -> Option<PipelineState> {
        if v.len() != 31 {
            return None;
        }
        let u = |n: usize| u32::try_from(v[n]).ok();
        let face = |at: usize| -> Option<[u32; 4]> { Some([u(at)?, u(at + 1)?, u(at + 2)?, u(at + 3)?]) };
        Some(PipelineState {
            topology: *TOPOLOGIES.get(v[0] as usize)?,
            target_format: *FORMATS.get(v[1] as usize)?,
            depth_format: match v[2] {
                0 => None,
                d => Some(*FORMATS.get(d as usize - 1)?),
            },
            blend_on: u(3)?,
            masks: u(4)?,
            depth_test: v[5] != 0,
            depth_write: v[6] != 0,
            depth_func: u(7)?,
            cull: u8::try_from(v[8]).ok()?,
            front_face: u(9)?,
            blend: Blend {
                colour_src: u(10)?,
                colour_dst: u(11)?,
                colour_op: u(12)?,
                separate_alpha: v[13] != 0,
                alpha_src: u(14)?,
                alpha_dst: u(15)?,
                alpha_op: u(16)?,
            },
            stencil: (v[17] != 0).then_some((face(18)?, face(22)?, u(26)?, u(27)?)),
            bias: (u(28)? as i32, u(29)?, u(30)?),
        })
    }
}

/// Which pipeline a draw needs, whatever addresses its shaders have: a hash of the two shaders' WGSL, how many colours the pixel shader
/// writes, and the state.
pub(crate) fn pipeline_id(vs: u64, ps: u64, outputs: usize, state: &PipelineState) -> u64 {
    let mut words: Vec<u64> = vec![vs, ps, outputs as u64];
    match state.encode() {
        Some(v) => words.extend(v),
        // not one the disk knows: a hash of the state stands in (these ids are never written down)
        None => words.extend([u64::MAX, crate::draw::hash_of(state)]),
    }
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    xxhash_rust::xxh3::xxh3_64(&bytes)
}

/// A made pipeline and the layouts of its four bind groups (asked for once).
#[derive(Clone)]
pub(crate) struct Built {
    pub pipeline: wgpu::RenderPipeline,
    pub layouts: [wgpu::BindGroupLayout; 4],
}

/// Makes the render pipeline (the one place that turns a state into a `wgpu` description).
pub(crate) fn build(device: &wgpu::Device, vs: &wgpu::ShaderModule, ps: &wgpu::ShaderModule, outputs: usize, st: &PipelineState) -> Built {
    use crate::draw::{blend_factor, blend_op, blends, compare, stencil_face};
    let blend_on = st.blend_on & 1 != 0 && blends(st.target_format);
    let b = &st.blend;
    let colour = wgpu::BlendComponent {
        src_factor: blend_factor(b.colour_src),
        dst_factor: blend_factor(b.colour_dst),
        operation: blend_op(b.colour_op),
    };
    let blend = blend_on.then(|| wgpu::BlendState {
        color: colour,
        alpha: if b.separate_alpha {
            wgpu::BlendComponent {
                src_factor: blend_factor(b.alpha_src),
                dst_factor: blend_factor(b.alpha_dst),
                operation: blend_op(b.alpha_op),
            }
        } else {
            colour
        },
    });
    let mask = wgpu::ColorWrites::from_bits_truncate(st.masks & 0xf);
    let targets: Vec<Option<wgpu::ColorTargetState>> = (0..outputs)
        .map(|_| {
            Some(wgpu::ColorTargetState {
                format: st.target_format,
                blend,
                write_mask: mask,
            })
        })
        .collect();
    let strip = matches!(
        st.topology,
        wgpu::PrimitiveTopology::TriangleStrip | wgpu::PrimitiveTopology::LineStrip
    );
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("game pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: vs,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: ps,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        primitive: wgpu::PrimitiveState {
            topology: st.topology,
            strip_index_format: strip.then_some(wgpu::IndexFormat::Uint32),
            front_face: if st.front_face == 0 {
                wgpu::FrontFace::Ccw
            } else {
                wgpu::FrontFace::Cw
            },
            cull_mode: match st.cull {
                0 => None,
                2 => Some(wgpu::Face::Front),
                _ => Some(wgpu::Face::Back),
            },
            ..Default::default()
        },
        depth_stencil: st.depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(st.depth_write),
            depth_compare: Some(if st.depth_test {
                compare(st.depth_func)
            } else {
                wgpu::CompareFunction::Always
            }),
            stencil: st
                .stencil
                .map_or_else(Default::default, |(front, back, read, write)| wgpu::StencilState {
                    front: stencil_face(front),
                    back: stencil_face(back),
                    read_mask: read,
                    write_mask: write,
                }),
            bias: wgpu::DepthBiasState {
                constant: st.bias.0,
                slope_scale: f32::from_bits(st.bias.1),
                clamp: f32::from_bits(st.bias.2),
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let layouts = std::array::from_fn(|n| pipeline.get_bind_group_layout(n as u32));
    Built { pipeline, layouts }
}

/// A shader for a worker: the module the draw made, or WGSL from the disk cache (made into a module once, shared by the workers).
enum Source {
    Module(wgpu::ShaderModule),
    Wgsl(u64, Arc<str>),
}

struct Job {
    id: u64,
    vs: Source,
    ps: Source,
    outputs: usize,
    state: PipelineState,
    /// The shaders' ids and WGSL, to write the pipeline down once made (not for one that came from the disk).
    record: Option<(u64, Arc<str>, u64, Arc<str>)>,
}

struct Done {
    id: u64,
    built: Built,
    ms: f64,
}

/// What the pipelines did since the counters were last taken (for the timing report and the area-change lines).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PipelineCounts {
    /// Made on this thread, with the draw waiting.
    pub made_here: u64,
    pub made_here_ms: f64,
    /// Made by the workers (time on their threads, not the frame's).
    pub made_by_workers: u64,
    pub workers_ms: f64,
    /// Handed to the workers.
    pub queued: u64,
    /// Draws not drawn because their pipeline was being made.
    pub draws_waiting: u64,
    /// Found among those made from the disk cache or by the workers, without making one.
    pub found_made: u64,
}

/// The disk cache's folder and its open list.
struct Disk {
    dir: PathBuf,
    list: Mutex<std::fs::File>,
    /// Shaders already written (by id).
    written: Mutex<HashSet<u64>>,
}

impl Disk {
    fn write_shader(&self, id: u64, wgsl: &str) {
        let mut written = self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !written.insert(id) {
            return;
        }
        let path = self.dir.join("shaders").join(format!("{id:016x}.wgsl"));
        if path.exists() {
            return;
        }
        // written whole under another name and renamed, so a run that stops halfway leaves no half shader
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, wgsl).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn record(&self, id: u64, vs: (u64, &str), ps: (u64, &str), outputs: usize, state: &PipelineState) {
        let Some(words) = state.encode() else { return };
        self.write_shader(vs.0, vs.1);
        self.write_shader(ps.0, ps.1);
        let mut line = format!("p1 {id:016x} {:016x} {:016x} {outputs}", vs.0, ps.0);
        for w in words {
            line.push_str(&format!(" {w:x}"));
        }
        line.push('\n');
        let mut f = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = f.write_all(line.as_bytes());
    }
}

/// One line of `pipelines.txt`: (id, vertex shader id, pixel shader id, colour outputs, state).
pub(crate) fn parse_line(line: &str) -> Option<(u64, u64, u64, usize, PipelineState)> {
    let mut it = line.split_ascii_whitespace();
    if it.next()? != "p1" {
        return None;
    }
    let id = u64::from_str_radix(it.next()?, 16).ok()?;
    let vs = u64::from_str_radix(it.next()?, 16).ok()?;
    let ps = u64::from_str_radix(it.next()?, 16).ok()?;
    let outputs: usize = it.next()?.parse().ok()?;
    let words: Option<Vec<u64>> = it.map(|w| u64::from_str_radix(w, 16).ok()).collect();
    let state = PipelineState::decode(&words?)?;
    // a line that does not hash to its id (cut short, or edited) is not used
    (pipeline_id(vs, ps, outputs, &state) == id && outputs <= 8).then_some((id, vs, ps, outputs, state))
}

/// The most pipelines read back from the disk at a start.
const DISK_LIMIT: usize = 50_000;

fn env_off(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "off" | "no" | "false"))
}

/// The pipelines: made, being made, and the workers and disk cache that make and keep them.
pub(crate) struct Pipelines {
    device: wgpu::Device,
    pub asynchronous: bool,
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    done_tx: Sender<Done>,
    pending: HashSet<u64>,
    made: HashMap<u64, Built>,
    /// Shared with the workers, which write down what they make (set once, when the disk cache is opened).
    disk: Arc<OnceLock<Disk>>,
    /// Pipelines already in the disk cache (by id), not written again.
    known: HashSet<u64>,
    pub counts: PipelineCounts,
    /// Pipelines read from the disk at the start and not made yet.
    pub prebuilding: usize,
    /// Shader modules by the hash of their WGSL, shared with the workers: the ones the workers make for the pipelines read from the
    /// disk serve the draws too (a translation found on disk then costs no module).
    modules: Modules,
}

type Modules = Arc<Mutex<HashMap<u64, wgpu::ShaderModule>>>;

/// The module for WGSL with hash `id`: the one made before, or made now (outside the lock, so two threads making modules do not wait
/// for each other). The second value says whether it was there.
fn module_for(device: &wgpu::Device, modules: &Modules, id: u64, wgsl: &str, label: &str) -> (wgpu::ShaderModule, bool) {
    if let Some(m) = modules.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id) {
        return (m.clone(), true);
    }
    let m = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let mut all = modules.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (all.entry(id).or_insert(m).clone(), false)
}

impl Pipelines {
    pub(crate) fn new(device: wgpu::Device) -> Pipelines {
        let asynchronous = !env_off("MH3U_GFX_ASYNC_PIPELINES");
        let (done_tx, done) = channel();
        Pipelines {
            device,
            asynchronous,
            jobs: None,
            done,
            done_tx,
            pending: HashSet::new(),
            made: HashMap::new(),
            disk: Arc::default(),
            known: HashSet::new(),
            counts: PipelineCounts::default(),
            prebuilding: 0,
            modules: Arc::default(),
        }
    }

    /// The shader module for WGSL with hash `id` (see `modules`), and whether it was made already.
    pub(crate) fn module(&self, id: u64, wgsl: &str, label: &str) -> (wgpu::ShaderModule, bool) {
        module_for(&self.device, &self.modules, id, wgsl, label)
    }

    /// The workers, started at the first job: a few threads (half the cores, at most three: the game's own threads and the render
    /// thread need the rest).
    fn workers(&mut self) -> &Sender<Job> {
        if self.jobs.is_none() {
            let (tx, rx) = channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            let modules = self.modules.clone();
            let n = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 3);
            for k in 0..n {
                let (rx, done, device, modules, disk) = (
                    rx.clone(),
                    self.done_tx.clone(),
                    self.device.clone(),
                    modules.clone(),
                    self.disk.clone(),
                );
                let _ = std::thread::Builder::new()
                    .name(format!("mh3u-pipelines-{k}"))
                    .spawn(move || work(&rx, &done, &device, &modules, &disk));
            }
            self.jobs = Some(tx);
        }
        self.jobs.as_ref().expect("made above")
    }

    /// Takes in what the workers have made.
    pub(crate) fn collect(&mut self) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&d.id);
            self.counts.made_by_workers += 1;
            self.counts.workers_ms += d.ms;
            self.prebuilding = self.prebuilding.saturating_sub(usize::from(!self.made.contains_key(&d.id)));
            self.made.insert(d.id, d.built);
        }
    }

    /// The pipeline for a draw's shaders and state: made already, or made now (the draw waits), or handed to a worker (None: the draw
    /// is not drawn this time).
    pub(crate) fn get(&mut self, vs: &Compiled, ps: &Compiled, state: &PipelineState) -> Option<Built> {
        self.collect();
        let outputs = ps.interface.colour_outputs.len();
        let id = pipeline_id(vs.id, ps.id, outputs, state);
        if let Some(b) = self.made.get(&id) {
            self.counts.found_made += 1;
            return Some(b.clone());
        }
        let record = (self.disk.get().is_some() && !self.known.contains(&id) && state.encode().is_some())
            .then(|| (vs.id, vs.wgsl.clone(), ps.id, ps.wgsl.clone()));
        if self.asynchronous {
            self.counts.draws_waiting += 1;
            if self.pending.insert(id) {
                self.known.insert(id);
                self.counts.queued += 1;
                let job = Job {
                    id,
                    vs: Source::Module(vs.module.clone()),
                    ps: Source::Module(ps.module.clone()),
                    outputs,
                    state: state.clone(),
                    record,
                };
                let _ = self.workers().send(job);
            }
            return None;
        }
        let started = Instant::now();
        let built = build(&self.device, &vs.module, &ps.module, outputs, state);
        self.counts.made_here += 1;
        self.counts.made_here_ms += started.elapsed().as_secs_f64() * 1000.0;
        if let (Some(disk), Some((vi, vw, pi, pw))) = (self.disk.get(), record) {
            disk.record(id, (vi, &vw), (pi, &pw), outputs, state);
            self.known.insert(id);
        }
        self.made.insert(id, built.clone());
        Some(built)
    }

    /// Opens (or starts) the disk cache under `root` for this adapter and build, and hands every pipeline in it to the workers.
    /// Returns the folder and how many pipelines were queued.
    pub(crate) fn open_disk(&mut self, root: &Path, adapter: &wgpu::AdapterInfo) -> std::io::Result<(PathBuf, usize)> {
        let about = format!(
            "graphics sources {}\nadapter {}\nvendor {:#x} device {:#x}\ndriver {} {}\nbackend {:?}\n",
            crate::SHADER_SOURCE,
            adapter.name,
            adapter.vendor,
            adapter.device,
            adapter.driver,
            adapter.driver_info,
            adapter.backend
        );
        let key = xxhash_rust::xxh3::xxh3_64(about.as_bytes());
        let dir = root.join("pipelines").join(format!("{key:016x}"));
        std::fs::create_dir_all(dir.join("shaders"))?;
        std::fs::write(dir.join("about.txt"), &about)?;
        let list_path = dir.join("pipelines.txt");
        let text = std::fs::read_to_string(&list_path).unwrap_or_default();
        let list = std::fs::OpenOptions::new().create(true).append(true).open(&list_path)?;
        let _ = self.disk.set(Disk {
            dir: dir.clone(),
            list: Mutex::new(list),
            written: Mutex::new(HashSet::new()),
        });
        let mut shaders: HashMap<u64, Option<Arc<str>>> = HashMap::new();
        let mut read = |id: u64| -> Option<Arc<str>> {
            shaders
                .entry(id)
                .or_insert_with(|| {
                    std::fs::read_to_string(dir.join("shaders").join(format!("{id:016x}.wgsl")))
                        .ok()
                        .map(Arc::from)
                })
                .clone()
        };
        let mut jobs = Vec::new();
        for line in text.lines() {
            if jobs.len() >= DISK_LIMIT {
                break;
            }
            let Some((id, vs, ps, outputs, state)) = parse_line(line) else {
                continue;
            };
            if !self.known.insert(id) {
                continue;
            }
            let (Some(vw), Some(pw)) = (read(vs), read(ps)) else { continue };
            jobs.push(Job {
                id,
                vs: Source::Wgsl(vs, vw),
                ps: Source::Wgsl(ps, pw),
                outputs,
                state,
                record: None,
            });
        }
        let n = jobs.len();
        for job in jobs {
            self.pending.insert(job.id);
            let _ = self.workers().send(job);
        }
        self.prebuilding = n;
        Ok((dir, n))
    }

    pub(crate) fn sizes(&self) -> (usize, usize) {
        (self.made.len(), self.pending.len())
    }

    /// Waits for the workers to finish what they have (the tests).
    #[cfg(test)]
    pub(crate) fn wait(&mut self) {
        while !self.pending.is_empty() {
            match self.done.recv_timeout(std::time::Duration::from_secs(30)) {
                Ok(d) => {
                    self.pending.remove(&d.id);
                    self.counts.made_by_workers += 1;
                    self.prebuilding = self.prebuilding.saturating_sub(1);
                    self.made.insert(d.id, d.built);
                }
                Err(_) => panic!("a worker did not finish"),
            }
        }
    }
}

fn work(rx: &Mutex<Receiver<Job>>, done: &Sender<Done>, device: &wgpu::Device, modules: &Modules, disk: &OnceLock<Disk>) {
    let module = |s: Source| -> wgpu::ShaderModule {
        match s {
            Source::Module(m) => m,
            Source::Wgsl(id, wgsl) => module_for(device, modules, id, &wgsl, "shader from the pipeline cache").0,
        }
    };
    loop {
        let job = {
            let rx = rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match rx.recv() {
                Ok(j) => j,
                Err(_) => return,
            }
        };
        let started = Instant::now();
        let (vs, ps) = (module(job.vs), module(job.ps));
        let built = build(device, &vs, &ps, job.outputs, &job.state);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        if let (Some(disk), Some((vi, vw, pi, pw))) = (disk.get(), &job.record) {
            disk.record(job.id, (*vi, vw), (*pi, pw), job.outputs, &job.state);
        }
        if done.send(Done { id: job.id, built, ms }).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: &str = "@vertex fn main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }";
    const PS: &str = "@fragment fn main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";

    fn shader(device: &wgpu::Device, wgsl: &str, outputs: bool) -> Compiled {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let mut interface = mh3u_latte::translate::Interface::default();
        if outputs {
            interface.colour_outputs.insert(0);
        }
        Compiled::new(module, interface, wgsl.to_string())
    }

    fn state() -> PipelineState {
        PipelineState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            target_format: wgpu::TextureFormat::Rgba8Unorm,
            blend: Blend {
                colour_src: 4,
                colour_dst: 5,
                colour_op: 0,
                separate_alpha: true,
                alpha_src: 1,
                alpha_dst: 0,
                alpha_op: 0,
            },
            depth_format: Some(wgpu::TextureFormat::Depth24PlusStencil8),
            blend_on: 1,
            masks: 0xf,
            depth_test: true,
            depth_write: true,
            depth_func: 3,
            cull: 1,
            front_face: 0,
            stencil: Some(([7, 0, 0, 2], [3, 1, 1, 1], 0xff, 0x0f)),
            bias: (-192, 0, 0),
        }
    }

    #[test]
    fn a_state_goes_to_disk_and_back_unchanged() {
        let mut s = state();
        assert_eq!(PipelineState::decode(&s.encode().unwrap()), Some(s.clone()));
        s.stencil = None;
        s.depth_format = None;
        s.target_format = wgpu::TextureFormat::R32Float;
        s.topology = wgpu::PrimitiveTopology::TriangleStrip;
        s.bias = (0, 1.5f32.to_bits(), 0);
        assert_eq!(PipelineState::decode(&s.encode().unwrap()), Some(s.clone()));
        // a format the table does not have is not written down
        s.target_format = wgpu::TextureFormat::Bgra8Unorm;
        assert_eq!(s.encode(), None);
    }

    #[test]
    fn a_line_is_used_only_when_it_hashes_to_its_id() {
        let s = state();
        let id = pipeline_id(1, 2, 1, &s);
        let mut line = format!("p1 {id:016x} {:016x} {:016x} 1", 1, 2);
        for w in s.encode().unwrap() {
            line.push_str(&format!(" {w:x}"));
        }
        assert_eq!(parse_line(&line).map(|l| l.4), Some(s));
        // cut short, or a number changed
        assert_eq!(parse_line(&line[..line.len() - 2]), None);
        assert_eq!(parse_line(&line.replacen(" 1 ", " 2 ", 1)), None);
        assert_eq!(parse_line("p0 whatever"), None);
    }

    #[test]
    fn on_workers_the_draw_waits_a_frame_and_then_finds_its_pipeline() {
        let d = crate::Display::noop();
        let mut p = Pipelines::new(d.device.clone());
        p.asynchronous = true;
        let (vs, ps) = (shader(&d.device, VS, false), shader(&d.device, PS, true));
        assert!(p.get(&vs, &ps, &state()).is_none(), "handed to a worker");
        // the worker may already be done (the no-op device is quick): found then, but never handed to a worker twice
        let early = p.get(&vs, &ps, &state()).is_some();
        assert_eq!((p.counts.queued, p.counts.draws_waiting), (1, if early { 1 } else { 2 }));
        p.wait();
        assert!(p.get(&vs, &ps, &state()).is_some());
        assert_eq!(
            (p.counts.made_by_workers, p.counts.made_here, p.counts.found_made),
            (1, 0, if early { 2 } else { 1 })
        );
        // without the workers, the draw waits for it
        let mut p = Pipelines::new(d.device.clone());
        p.asynchronous = false;
        assert!(p.get(&vs, &ps, &state()).is_some());
        assert_eq!(p.counts.made_here, 1);
    }

    #[test]
    fn pipelines_made_in_one_run_are_made_again_at_the_next_start() {
        let d = crate::Display::noop();
        let root = std::env::temp_dir().join(format!("mh3u-pipelines-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let info = d.adapter.get_info();
        let (vs, ps) = (shader(&d.device, VS, false), shader(&d.device, PS, true));
        {
            let mut p = Pipelines::new(d.device.clone());
            p.asynchronous = false;
            let (dir, n) = p.open_disk(&root, &info).unwrap();
            assert_eq!(n, 0);
            assert!(p.get(&vs, &ps, &state()).is_some());
            let mut other = state();
            other.cull = 0;
            assert!(p.get(&vs, &ps, &other).is_some());
            assert_eq!(std::fs::read_to_string(dir.join("pipelines.txt")).unwrap().lines().count(), 2);
            assert_eq!(std::fs::read_dir(dir.join("shaders")).unwrap().count(), 2);
            assert!(
                std::fs::read_to_string(dir.join("about.txt"))
                    .unwrap()
                    .contains(crate::SHADER_SOURCE)
            );
        }
        // the next run: both are made on the workers at the start, and the draw finds them without making one
        let mut p = Pipelines::new(d.device.clone());
        p.asynchronous = true;
        let (_, n) = p.open_disk(&root, &info).unwrap();
        assert_eq!(n, 2);
        p.wait();
        assert_eq!(p.prebuilding, 0);
        // shaders made again in the new run (other modules, other addresses): found by their WGSL
        let (vs2, ps2) = (shader(&d.device, VS, false), shader(&d.device, PS, true));
        assert!(p.get(&vs2, &ps2, &state()).is_some());
        assert_eq!((p.counts.found_made, p.counts.queued, p.counts.made_here), (1, 0, 0));
        // and nothing is written twice
        let dir = std::fs::read_dir(root.join("pipelines")).unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read_to_string(dir.join("pipelines.txt")).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
