// The Blanket logo mark, rasterized in plain Rust with no dependencies so that both
// the browser (window icons) and `build.rs` (the `.exe` icon) can use it.

/// RGBA pixels of the Blanket logo mark: a dark rounded square holding a folded silk
/// wave, with the green accent dot. Rendered analytically with anti-aliasing.
#[allow(clippy::needless_range_loop)]
pub fn app_icon_rgba(size: u32) -> (Vec<u8>, u32) {
    let s = size as f32;
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let radius = s * 0.22;
    let background = [0x15u8, 0x18, 0x1d];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Rounded-square coverage.
            let dx = (px - s / 2.0).abs() - (s / 2.0 - radius);
            let dy = (py - s / 2.0).abs() - (s / 2.0 - radius);
            let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - radius;
            let shape = (0.5 - outside).clamp(0.0, 1.0);
            if shape <= 0.0 {
                continue;
            }
            let mut color = [background[0] as f32, background[1] as f32, background[2] as f32];
            // Three silk strands: sine bands with soft edges.
            let u = px / s;
            let v = py / s;
            for (i, offset) in [0.40f32, 0.52, 0.64].iter().enumerate() {
                let center = offset + 0.09 * (u * std::f32::consts::TAU * 0.9 + i as f32 * 0.7).sin();
                let distance = ((v - center) * s).abs();
                let width = s * 0.045;
                let coverage = (1.0 - (distance - width).max(0.0) / 1.2).clamp(0.0, 1.0);
                let shade = 0.62 + 0.38 * (1.0 - i as f32 * 0.25) * (0.6 + 0.4 * (u * std::f32::consts::PI).sin());
                let silver = [216.0 * shade, 222.0 * shade, 230.0 * shade];
                for channel in 0..3 {
                    color[channel] += (silver[channel] - color[channel]) * coverage;
                }
            }
            // Accent dot, top right.
            let dot = ((px - s * 0.74).powi(2) + (py - s * 0.26).powi(2)).sqrt() - s * 0.075;
            let dot_coverage = (0.5 - dot).clamp(0.0, 1.0);
            let accent = [0x3d as f32, 0xd6 as f32, 0x8c as f32];
            for channel in 0..3 {
                color[channel] += (accent[channel] - color[channel]) * dot_coverage;
            }
            let index = ((y * size + x) * 4) as usize;
            pixels[index] = color[0] as u8;
            pixels[index + 1] = color[1] as u8;
            pixels[index + 2] = color[2] as u8;
            pixels[index + 3] = (shape * 255.0) as u8;
        }
    }
    (pixels, size)
}
