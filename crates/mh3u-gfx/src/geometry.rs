//! Geometry pipelines: a vertex shader that writes a ring, a geometry shader that reads it and emits vertices into another, and the copy
//! shader that reads those and feeds the rasteriser. The hardware does this with fixed rings; here the first two stages are compute
//! shaders (the translator makes them from the same microcode) that write storage buffers, the geometry shader also writes the list of
//! triangles that its strips make, and the copy shader is an ordinary vertex shader that reads the second ring.

use crate::draw::{AttribStream, BufRef, Compiled, GeometryEntry, Role, count_lookup, hash_fast, hash_of, hash_ranges};
use crate::gx2::Gx2;
use crate::state::State;
use mh3u_latte::decode::{decode, ring_extent};
use mh3u_latte::translate::{Context, RingConfig, TexKind, VertexAttribute, VertexInputs, translate_es, translate_gs, translate_vertex};
use mh3u_rt::Guest;
use std::sync::Arc;

/// What the render stage of a geometry pipeline draws from.
pub struct GeoOut {
    pub copy: Arc<Compiled>,
    pub ring_out: BufRef,
    pub indices: BufRef,
    pub index_count: u32,
    /// Where the geometry shader's description of the vertex outputs' meanings (semantic numbers) is.
    pub semantics_at: u32,
}

impl Gx2 {
    fn read_program(g: &Guest, (program, size): (u32, u32)) -> Vec<u8> {
        let mut code = vec![0u8; size as usize];
        g.read(program, &mut code);
        code
    }

    fn compute(&self, module: &wgpu::ShaderModule) -> wgpu::ComputePipeline {
        self.d.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("geometry stage"),
            layout: None,
            module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn geometry_pass(
        &mut self,
        g: &Guest,
        st: &State,
        fetch: &Arc<[AttribStream]>,
        primitive: u32,
        count: u32,
        first: u32,
        indexed: Option<(u32, u32)>,
        instances: u32,
    ) -> Option<GeoOut> {
        if primitive != 1 || indexed.is_some() || instances > 1 {
            self.caches_report(format!(
                "a geometry pipeline draw of primitive {primitive} (indexed {}, {instances} instances) is not drawn",
                indexed.is_some()
            ));
            return None;
        }
        let (vs, gs) = (st.vertex_shader, st.geometry_shader);
        let es_vec4 = g.u32(vs + 0x10c) / 4;
        let gs_vec4 = g.u32(gs + 0x14) / 4;
        if es_vec4 == 0 || gs_vec4 == 0 {
            self.caches_report(format!("a geometry pipeline with ring items of {es_vec4} and {gs_vec4} vec4s"));
            return None;
        }
        let program = |size_at: u32, program_at: u32| (g.u32(program_at), g.u32(size_at).min(1 << 20));
        let ranges = [
            program(vs + 0xd0, vs + 0xd4),
            program(gs + 0x4c, gs + 0x50),
            program(gs + 0x54, gs + 0x58),
        ];
        let front = (
            vs,
            gs,
            ranges,
            Arc::as_ptr(fetch).cast::<u8>() as usize,
            st.half_z,
            es_vec4,
            gs_vec4,
        );
        let swaps = self.swaps;
        let (found, how) = self.caches.geometry_front.get(&front, swaps, |r| hash_ranges(g, r));
        count_lookup(
            &how,
            "geometry pipeline hits",
            "geometry pipeline checks",
            "geometry pipeline misses",
        );
        let entry = match found {
            Some((entry, _)) => entry,
            None => {
                let entry = self.geometry_slow(g, st, fetch, ranges, es_vec4, gs_vec4);
                let hash = hash_ranges(g, &ranges);
                self.caches
                    .geometry_front
                    .insert(front, ranges, hash, swaps, (entry.clone(), fetch.clone()));
                entry
            }
        }?;
        let (es, gst, copy, max_vertices) = (&entry.es, &entry.gs, entry.copy.clone(), entry.max_vertices);
        let (es_pipeline, gs_pipeline) = (&entry.es_pipeline, &entry.gs_pipeline);
        let device = self.d.device.clone();
        let total = first + count;
        // the vertex streams, as for any vertex shader
        let (stream_data, meta) = self.decode_streams(g, st, fetch, 0, total.min(1 << 22), 1);
        let va = self.arena_write(&stream_data, stream_data.len() as u64);
        let meta_buffer = self.arena_write(&meta.iter().flatten().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), 256);
        let ring_in = self.arena_alloc(Role::RingIn, u64::from(total) * u64::from(es_vec4) * 16);
        // what the geometry shader does not write must read as zero (an empty triangle)
        let ring_out = self.arena_alloc(Role::Out, u64::from(count) * u64::from(max_vertices) * u64::from(gs_vec4) * 16);
        let index_count = count * max_vertices * 3;
        let indices = self.arena_alloc(Role::Out, u64::from(index_count) * 4);
        let es_params = self.arena_write(&[total.to_le_bytes(), 0u32.to_le_bytes(), [0; 4], [0; 4]].concat(), 16);
        let gs_params = self.arena_write(&[count.to_le_bytes(), first.to_le_bytes(), [0; 4], [0; 4]].concat(), 16);
        // uniform blocks of each stage
        let mut keep: Vec<BufRef> = Vec::new();
        for (&bank, &len) in &es.interface.banks {
            keep.push(self.uniform_buffer(g, &st.vertex_blocks[(bank as usize).min(15)], len));
        }
        let es_banks = keep.len();
        for (&bank, &len) in &gst.interface.banks {
            keep.push(self.uniform_buffer(g, &st.geometry_blocks[(bank as usize).min(15)], len));
        }
        let es_consts: Vec<wgpu::BindGroupEntry> = es
            .interface
            .banks
            .keys()
            .enumerate()
            .map(|(i, &bank)| wgpu::BindGroupEntry {
                binding: bank,
                resource: keep[i].binding(),
            })
            .collect();
        let gs_consts: Vec<wgpu::BindGroupEntry> = gst
            .interface
            .banks
            .keys()
            .enumerate()
            .map(|(i, &bank)| wgpu::BindGroupEntry {
                binding: bank,
                resource: keep[es_banks + i].binding(),
            })
            .collect();
        let bind = |p: &wgpu::ComputePipeline, group: u32, entries: &[wgpu::BindGroupEntry]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p.get_bind_group_layout(group),
                entries,
            })
        };
        let es_g0 = bind(es_pipeline, 0, &es_consts);
        let es_g1 = bind(
            es_pipeline,
            1,
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: va.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: meta_buffer.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ring_in.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: es_params.binding(),
                },
            ],
        );
        let gs_g0 = bind(gs_pipeline, 0, &gs_consts);
        let gs_g1 = bind(
            gs_pipeline,
            1,
            &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ring_in.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: ring_out.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: indices.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: gs_params.binding(),
                },
            ],
        );
        let enc = self
            .batch
            .pre
            .get_or_insert_with(|| device.create_command_encoder(&Default::default()));
        enc.clear_buffer(&ring_out.buffer, ring_out.offset, Some(ring_out.size));
        enc.clear_buffer(&indices.buffer, indices.offset, Some(indices.size));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("es"),
                timestamp_writes: None,
            });
            pass.set_pipeline(es_pipeline);
            pass.set_bind_group(0, &es_g0, &[]);
            pass.set_bind_group(1, &es_g1, &[]);
            pass.dispatch_workgroups(total.div_ceil(64), 1, 1);
        }
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gs"),
                timestamp_writes: None,
            });
            pass.set_pipeline(gs_pipeline);
            pass.set_bind_group(0, &gs_g0, &[]);
            pass.set_bind_group(1, &gs_g1, &[]);
            pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
        }
        Some(GeoOut {
            copy,
            ring_out,
            indices,
            index_count,
            semantics_at: gs + 0x20,
        })
    }

    /// The geometry pipeline from its program bytes: translated once for each set of programs and inputs.
    fn geometry_slow(
        &mut self,
        g: &Guest,
        st: &State,
        fetch: &[AttribStream],
        ranges: [(u32, u32); 3],
        es_vec4: u32,
        gs_vec4: u32,
    ) -> Option<Arc<GeometryEntry>> {
        let (vs, gs) = (st.vertex_shader, st.geometry_shader);
        let es_code = Self::read_program(g, ranges[0]);
        let gs_code = Self::read_program(g, ranges[1]);
        let copy_code = Self::read_program(g, ranges[2]);
        let key = (
            vs,
            gs,
            hash_of(&(
                hash_fast(&es_code, 1),
                hash_fast(&gs_code, 2),
                hash_fast(&copy_code, 3),
                fetch,
                st.half_z,
                es_vec4,
                gs_vec4,
            )),
        );
        if let Some(e) = self.caches.geometry.get(&key) {
            return e.clone();
        }
        let result = (|| {
            let es_d = decode(&es_code).map_err(|e| format!("vertex shader: {e}"))?;
            let gs_d = decode(&gs_code).map_err(|e| format!("geometry shader: {e}"))?;
            let copy_d = decode(&copy_code).map_err(|e| format!("copy shader: {e}"))?;
            let max_vertices = ring_extent(&gs_d).div_ceil(gs_vec4 * 4).max(1);
            let ring = RingConfig {
                es_vec4,
                gs_vec4,
                max_vertices,
            };
            let ctx = Context {
                texture_kind: &|_| TexKind::D2,
            };
            let attributes = fetch
                .iter()
                .enumerate()
                .map(|(i, a)| VertexAttribute {
                    gpr: a.location + 1,
                    stream: i as u32,
                    per_instance: a.per_instance,
                    divisor: a.divisor,
                })
                .collect();
            let inputs = VertexInputs {
                attributes,
                half_z: st.half_z,
                ring_out_vec4: 0,
            };
            let es = translate_es(&es_d, &inputs, ring, &ctx).map_err(|e| format!("vertex shader: {e}"))?;
            let gst = translate_gs(&gs_d, ring, &ctx).map_err(|e| format!("geometry shader: {e}"))?;
            let copy_inputs = VertexInputs {
                attributes: Vec::new(),
                half_z: st.half_z,
                ring_out_vec4: gs_vec4,
            };
            let copy = translate_vertex(&copy_d, &copy_inputs, &ctx).map_err(|e| format!("copy shader: {e}"))?;
            Ok::<_, String>((es, gst, copy, max_vertices))
        })();
        let entry = match result {
            Ok((es, gst, copy, max_vertices)) => {
                let module = |label: &str, wgsl: String| {
                    self.d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some(label),
                        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
                    })
                };
                let (es_module, gs_module, copy_module) = (module("es", es.wgsl), module("gs", gst.wgsl), module("copy", copy.wgsl));
                let (es_pipeline, gs_pipeline) = (self.compute(&es_module), self.compute(&gs_module));
                Some(Arc::new(GeometryEntry {
                    es: Arc::new(Compiled {
                        module: es_module,
                        interface: es.interface,
                    }),
                    gs: Arc::new(Compiled {
                        module: gs_module,
                        interface: gst.interface,
                    }),
                    copy: Arc::new(Compiled {
                        module: copy_module,
                        interface: copy.interface,
                    }),
                    max_vertices,
                    es_pipeline,
                    gs_pipeline,
                }))
            }
            Err(e) => {
                self.caches_report(format!("geometry pipeline {gs:#x} not translated: {e}"));
                None
            }
        };
        self.caches.geometry.insert(key, entry.clone());
        entry
    }
}
