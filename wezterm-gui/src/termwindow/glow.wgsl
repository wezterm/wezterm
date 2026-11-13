// Glow post-processing shaders for WezTerm
// Implements neon glow effect via post-processing

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex: vec2<f32>,
    @location(2) fg_color: vec4<f32>,
    @location(3) alt_color: vec4<f32>,
    @location(4) hsv: vec3<f32>,
    @location(5) has_color: f32,
    @location(6) mix_value: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
}

struct GlowUniform {
    radius: f32,
    threshold: f32,
    strength: f32,
    color_boost: f32,
    time_ms: u32,
    animation_enabled: u32,  // 0 = disabled, 1 = enabled
    animation_type: u32,     // 0 = pulse, 1 = shimmer, 2 = pulse+shimmer
    animation_speed: f32,    // Speed multiplier for animations
}

@group(0) @binding(0) var<uniform> glow_params: GlowUniform;
@group(1) @binding(0) var src_texture: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;

// Vertex shader for fullscreen quad
@vertex
fn vs_fullscreen(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.tex_coord = input.tex;
    return output;
}

// Extract bright areas from the main color buffer
@fragment
fn fs_extract(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(src_texture, src_sampler, input.tex_coord);
    
    // Calculate luminance with slight boost for saturated colors
    let boosted_color = color.rgb * glow_params.color_boost;
    let luminance = dot(boosted_color, vec3<f32>(0.2126, 0.7152, 0.0722));
    
    // Threshold the luminance
    let mask = select(0.0, 1.0, luminance > glow_params.threshold);
    
    // Return the color multiplied by the mask, preserving color information
    return vec4<f32>(color.rgb * mask, mask);
}

// Horizontal Gaussian blur
@fragment
fn fs_blur_h(input: VertexOutput) -> @location(0) vec4<f32> {
    let texture_size = vec2<f32>(textureDimensions(src_texture));
    let texel_size = 1.0 / texture_size;
    
    var result = vec4<f32>(0.0);
    var weight_sum = 0.0;
    
    // 9-tap Gaussian blur
    let radius = max(glow_params.radius, 0.001);
    let sigma = radius * 0.5;
    let sigma_sq = sigma * sigma;
    
    for (var i = -4; i <= 4; i = i + 1) {
        let offset = f32(i) * texel_size.x;
        let sample_coord = input.tex_coord + vec2<f32>(offset, 0.0);
        
        // Gaussian weight
        let x = f32(i);
        let weight = exp(-0.5 * (x * x) / sigma_sq);
        
        let sample_color = textureSample(src_texture, src_sampler, sample_coord);
        result += sample_color * weight;
        weight_sum += weight;
    }
    
    return result / weight_sum;
}

// Vertical Gaussian blur
@fragment
fn fs_blur_v(input: VertexOutput) -> @location(0) vec4<f32> {
    let texture_size = vec2<f32>(textureDimensions(src_texture));
    let texel_size = 1.0 / texture_size;
    
    var result = vec4<f32>(0.0);
    var weight_sum = 0.0;
    
    // 9-tap Gaussian blur
    let radius = max(glow_params.radius, 0.001);
    let sigma = radius * 0.5;
    let sigma_sq = sigma * sigma;
    
    for (var i = -4; i <= 4; i = i + 1) {
        let offset = f32(i) * texel_size.y;
        let sample_coord = input.tex_coord + vec2<f32>(0.0, offset);
        
        // Gaussian weight
        let x = f32(i);
        let weight = exp(-0.5 * (x * x) / sigma_sq);
        
        let sample_color = textureSample(src_texture, src_sampler, sample_coord);
        result += sample_color * weight;
        weight_sum += weight;
    }
    
    return result / weight_sum;
}

// Composite the blurred glow additively onto the main color buffer
@fragment
fn fs_composite(input: VertexOutput) -> @location(0) vec4<f32> {
    let glow_color = textureSample(src_texture, src_sampler, input.tex_coord);
    
    var final_strength = glow_params.strength;
    
    // Apply animation if enabled
    if (glow_params.animation_enabled != 0u) {
        let time_sec = f32(glow_params.time_ms) * 0.001 * glow_params.animation_speed;
        
        // Animation type: 0 = pulse, 1 = shimmer, 2 = pulse+shimmer
        if (glow_params.animation_type == 0u) {
            // Pulse only - slow breathing effect (3 second cycle at speed 1.0)
            let pulse_cycle = sin(time_sec * 2.094395) * 0.5 + 0.5; // 2π/3 ≈ 2.094395
            final_strength = glow_params.strength * (0.5 + pulse_cycle * 0.5); // Range: 50%-100%
        } else if (glow_params.animation_type == 1u) {
            // Shimmer only - faster subtle sparkle
            let shimmer_cycle = sin(time_sec * 8.377580) * 0.5 + 0.5; // 2π*4/3 ≈ 8.377580
            final_strength = glow_params.strength * (0.9 + shimmer_cycle * 0.1); // Range: 90%-100%
        } else if (glow_params.animation_type == 2u) {
            // Pulse + Shimmer - both effects combined
            let pulse_cycle = sin(time_sec * 2.094395) * 0.5 + 0.5;
            let shimmer_cycle = sin(time_sec * 8.377580) * 0.5 + 0.5;
            let pulse_strength = glow_params.strength * (0.5 + pulse_cycle * 0.5);
            final_strength = pulse_strength * (0.95 + shimmer_cycle * 0.05);
        }
    }
    
    // Apply final strength and return for additive blending
    return vec4<f32>(glow_color.rgb * final_strength, glow_color.a * final_strength);
}
