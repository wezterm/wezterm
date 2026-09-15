//! Glow post-processing effect for WebGPU backend
//! 
//! This module implements a neon glow effect by:
//! 1. Extracting bright glyph coverage to an offscreen texture
//! 2. Applying separable Gaussian blur (horizontal then vertical)
//! 3. Compositing the blurred result additively over the main color buffer

use crate::quad::Vertex;
use config::ExperimentalGlow;
use wgpu::util::DeviceExt;
use window::Dimensions;

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlowUniform {
    pub radius: f32,
    pub threshold: f32,
    pub strength: f32,
    pub color_boost: f32,
    pub time_ms: u32,
    pub animation_enabled: u32, // 0 = disabled, 1 = enabled
    pub animation_type: u32,    // 0 = pulse, 1 = shimmer, 2 = pulse+shimmer
    pub animation_speed: f32,   // Speed multiplier for animations
}

impl GlowUniform {
    pub fn from_config_with_time(config: &ExperimentalGlow, time_ms: u32) -> Self {
        use config::GlowAnimationType;
        
        let animation_type = match config.animation_type {
            GlowAnimationType::Pulse => 0,
            GlowAnimationType::Shimmer => 1,
            GlowAnimationType::PulseAndShimmer => 2,
        };
        
        Self {
            radius: config.radius,
            threshold: config.threshold,
            strength: config.strength,
            color_boost: config.color_boost,
            time_ms,
            animation_enabled: if config.animation_enabled { 1 } else { 0 },
            animation_type,
            animation_speed: config.animation_speed,
        }
    }
}

pub struct GlowRenderer {
    // Offscreen textures
    glow_src_texture: wgpu::Texture,
    glow_src_view: wgpu::TextureView,
    glow_tmp_texture: wgpu::Texture,
    glow_tmp_view: wgpu::TextureView,
    
    // Render pipelines
    extract_pipeline: wgpu::RenderPipeline,
    blur_h_pipeline: wgpu::RenderPipeline,
    blur_v_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    
    // Bind group layouts
    uniform_bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    
    // Samplers
    linear_sampler: wgpu::Sampler,
    
    // Vertex buffer for fullscreen quad
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    
    // Current dimensions
    width: u32,
    height: u32,
}

impl GlowRenderer {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat, dimensions: &Dimensions) -> anyhow::Result<Self> {
        let width = dimensions.pixel_width as u32;
        let height = dimensions.pixel_height as u32;
        
        // Create offscreen textures
        let (glow_src_texture, glow_src_view) = Self::create_offscreen_texture(device, width, height, "Glow Source")?;
        let (glow_tmp_texture, glow_tmp_view) = Self::create_offscreen_texture(device, width, height, "Glow Temp")?;
        
        // Create samplers
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        
        // Create bind group layouts
        let uniform_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Glow Uniform Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        
        let texture_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Glow Texture Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        
        // Load glow shaders
        let glow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Glow Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glow.wgsl").into()),
        });
        
        // Create render pipelines
        let extract_pipeline = Self::create_pipeline(
            device,
            &glow_shader,
            "vs_fullscreen",
            "fs_extract",
            &uniform_bind_group_layout,
            &texture_bind_group_layout,
            surface_format,
            "Glow Extract Pipeline",
        )?;
        
        let blur_h_pipeline = Self::create_pipeline(
            device,
            &glow_shader,
            "vs_fullscreen",
            "fs_blur_h",
            &uniform_bind_group_layout,
            &texture_bind_group_layout,
            surface_format,
            "Glow Blur H Pipeline",
        )?;
        
        let blur_v_pipeline = Self::create_pipeline(
            device,
            &glow_shader,
            "vs_fullscreen",
            "fs_blur_v",
            &uniform_bind_group_layout,
            &texture_bind_group_layout,
            surface_format,
            "Glow Blur V Pipeline",
        )?;
        
        let composite_pipeline = Self::create_pipeline(
            device,
            &glow_shader,
            "vs_fullscreen",
            "fs_composite",
            &uniform_bind_group_layout,
            &texture_bind_group_layout,
            surface_format,
            "Glow Composite Pipeline",
        )?;
        
        // Create fullscreen quad
        let (quad_vertex_buffer, quad_index_buffer) = Self::create_fullscreen_quad(device)?;
        
        Ok(Self {
            glow_src_texture,
            glow_src_view,
            glow_tmp_texture,
            glow_tmp_view,
            extract_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            composite_pipeline,
            uniform_bind_group_layout,
            texture_bind_group_layout,
            linear_sampler,
            quad_vertex_buffer,
            quad_index_buffer,
            width,
            height,
        })
    }
    
    pub fn resize(&mut self, device: &wgpu::Device, dimensions: &Dimensions) -> anyhow::Result<()> {
        let new_width = dimensions.pixel_width as u32;
        let new_height = dimensions.pixel_height as u32;
        
        if new_width != self.width || new_height != self.height {
            // Recreate offscreen textures
            let (glow_src_texture, glow_src_view) = Self::create_offscreen_texture(device, new_width, new_height, "Glow Source")?;
            let (glow_tmp_texture, glow_tmp_view) = Self::create_offscreen_texture(device, new_width, new_height, "Glow Temp")?;
            
            self.glow_src_texture = glow_src_texture;
            self.glow_src_view = glow_src_view;
            self.glow_tmp_texture = glow_tmp_texture;
            self.glow_tmp_view = glow_tmp_view;
            self.width = new_width;
            self.height = new_height;
        }
        
        Ok(())
    }
    
    pub fn render_glow(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        main_color_view: &wgpu::TextureView,
        glow_config: &ExperimentalGlow,
        time_ms: u32,
    ) -> anyhow::Result<()> {
        if !glow_config.enabled || glow_config.strength <= 0.0 || glow_config.radius <= 0.0 {
            return Ok(());
        }
        

        
        // Create uniform buffer
        let glow_uniform = GlowUniform::from_config_with_time(glow_config, time_ms);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Glow Uniform Buffer"),
            contents: bytemuck::cast_slice(&[glow_uniform]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Glow Uniform Bind Group"),
            layout: &self.uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        
        // Step 1: Extract bright areas from main color buffer to glow_src
        {
            let main_texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Main Texture Bind Group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(main_color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glow Extract Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.glow_src_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            
            render_pass.set_pipeline(&self.extract_pipeline);
            render_pass.set_bind_group(0, &uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &main_texture_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
        
        // Step 2: Horizontal blur from glow_src to glow_tmp
        {
            let glow_src_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Glow Src Bind Group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.glow_src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glow Blur H Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.glow_tmp_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            
            render_pass.set_pipeline(&self.blur_h_pipeline);
            render_pass.set_bind_group(0, &uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &glow_src_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
        
        // Step 3: Vertical blur from glow_tmp back to glow_src
        {
            let glow_tmp_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Glow Tmp Bind Group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.glow_tmp_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glow Blur V Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.glow_src_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            
            render_pass.set_pipeline(&self.blur_v_pipeline);
            render_pass.set_bind_group(0, &uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &glow_tmp_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
        
        // Step 4: Composite blurred glow additively onto main color buffer
        {
            let glow_src_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Glow Final Bind Group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.glow_src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glow Composite Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: main_color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            
            render_pass.set_pipeline(&self.composite_pipeline);
            render_pass.set_bind_group(0, &uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &glow_src_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
        
        Ok(())
    }
    
    fn create_offscreen_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        label: &str,
    ) -> anyhow::Result<(wgpu::Texture, wgpu::TextureView)> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok((texture, view))
    }
    
    fn create_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        vs_entry: &str,
        fs_entry: &str,
        uniform_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> anyhow::Result<wgpu::RenderPipeline> {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{} Layout", label)),
            bind_group_layouts: &[uniform_layout, texture_layout],
            push_constant_ranges: &[],
        });
        
        let blend_state = if fs_entry == "fs_composite" {
            // Additive blending for composite pass
            Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            })
        } else {
            // Normal blending for other passes
            Some(wgpu::BlendState::REPLACE)
        };
        
        Ok(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(vs_entry),
                buffers: &[Vertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fs_entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: blend_state,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        }))
    }
    
    fn create_fullscreen_quad(device: &wgpu::Device) -> anyhow::Result<(wgpu::Buffer, wgpu::Buffer)> {
        // Fullscreen quad vertices (NDC coordinates)
        let vertices = [
            Vertex {
                position: [-1.0, -1.0],
                tex: [0.0, 1.0],
                fg_color: [1.0, 1.0, 1.0, 1.0],
                alt_color: [0.0, 0.0, 0.0, 0.0],
                hsv: [1.0, 1.0, 1.0],
                has_color: 0.0,
                mix_value: 0.0,
            },
            Vertex {
                position: [1.0, -1.0],
                tex: [1.0, 1.0],
                fg_color: [1.0, 1.0, 1.0, 1.0],
                alt_color: [0.0, 0.0, 0.0, 0.0],
                hsv: [1.0, 1.0, 1.0],
                has_color: 0.0,
                mix_value: 0.0,
            },
            Vertex {
                position: [1.0, 1.0],
                tex: [1.0, 0.0],
                fg_color: [1.0, 1.0, 1.0, 1.0],
                alt_color: [0.0, 0.0, 0.0, 0.0],
                hsv: [1.0, 1.0, 1.0],
                has_color: 0.0,
                mix_value: 0.0,
            },
            Vertex {
                position: [-1.0, 1.0],
                tex: [0.0, 0.0],
                fg_color: [1.0, 1.0, 1.0, 1.0],
                alt_color: [0.0, 0.0, 0.0, 0.0],
                hsv: [1.0, 1.0, 1.0],
                has_color: 0.0,
                mix_value: 0.0,
            },
        ];
        
        let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
        
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Glow Quad Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Glow Quad Index Buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        
        Ok((vertex_buffer, index_buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::ExperimentalGlow;

    #[test]
    fn test_glow_uniform_from_config() {
        let config = ExperimentalGlow {
            enabled: true,
            radius: 3.0,
            strength: 0.8,
            threshold: 0.6,
            color_boost: 1.2,
            animation_enabled: true,
            animation_type: config::GlowAnimationType::Pulse,
            animation_speed: 2.0,
        };
        
        let uniform = GlowUniform::from_config_with_time(&config, 1000);
        assert_eq!(uniform.radius, 3.0);
        assert_eq!(uniform.strength, 0.8);
        assert_eq!(uniform.threshold, 0.6);
        assert_eq!(uniform.color_boost, 1.2);
        assert_eq!(uniform.time_ms, 1000);
        assert_eq!(uniform.animation_enabled, 1);
        assert_eq!(uniform.animation_type, 0); // Pulse = 0
        assert_eq!(uniform.animation_speed, 2.0);
    }

    #[test]
    fn test_glow_uniform_default() {
        let uniform = GlowUniform::default();
        assert_eq!(uniform.radius, 0.0);
        assert_eq!(uniform.strength, 0.0);
        assert_eq!(uniform.threshold, 0.0);
        assert_eq!(uniform.color_boost, 0.0);
    }
}