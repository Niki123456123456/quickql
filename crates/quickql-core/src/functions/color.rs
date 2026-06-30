/// Converts HSV to RGB.
///
/// h: hue in degrees, usually 0.0..360.0
/// s: saturation in 0.0..1.0
/// v: value/brightness in 0.0..1.0
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);

    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r1, g1, b1) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

fn van_der_corput_base2(index: u64) -> f64 {
    let mut n = index + 1;
    let mut result = 0.0;
    let mut denominator = 2.0;

    while n > 0 {
        let remainder = n % 2;
        result += remainder as f64 / denominator;

        n /= 2;
        denominator *= 2.0;
    }

    result
}

pub fn get_color(index: u64) -> String {
    let hue = van_der_corput_base2(index) as f32 * 360.0;
    let (r, g, b) = hsv_to_rgb(hue, 0.8, 1.0);
    format!("rgb({}, {}, {})", r, g, b)
}
