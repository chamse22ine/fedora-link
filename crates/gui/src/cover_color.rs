//! Couleur dominante d'une pochette : réduction à 64×64, k-means (4 clusters),
//! cluster le plus peuplé hors pixels quasi blancs/noirs, assombri pour la lisibilité du blanc.

use image::{imageops::FilterType, ImageReader, RgbImage};

pub const FALLBACK: &str = "#1c3a5e";
const MIN_CONTRAST: f32 = 4.5;

/// Couleur CSS (`#rrggbb`) à appliquer pour le fichier de pochette donné.
pub fn from_path(path: &str) -> Option<String> {
    let img = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let small = img.resize_exact(64, 64, FilterType::Triangle).to_rgb8();
    let rgb = readable(dominant(&small)?);
    Some(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
}

fn dominant(img: &RgbImage) -> Option<[f32; 3]> {
    let mut pixels: Vec<[f32; 3]> = img
        .pixels()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .filter(|p| {
            let (max, min) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            !(min >= 235.0 || max <= 20.0)
        })
        .collect();
    if pixels.len() < 16 {
        return None;
    }
    // Départ déterministe : quantiles de luminance.
    pixels.sort_by(|a, b| luma(a).total_cmp(&luma(b)));
    const K: usize = 4;
    let mut centers: Vec<[f32; 3]> = (0..K)
        .map(|i| pixels[(2 * i + 1) * pixels.len() / (2 * K)])
        .collect();
    let mut counts = [0usize; K];
    for _ in 0..8 {
        let mut sums = [[0f32; 3]; K];
        counts = [0; K];
        for p in &pixels {
            let c = nearest(&centers, p);
            counts[c] += 1;
            for ch in 0..3 {
                sums[c][ch] += p[ch];
            }
        }
        for c in 0..K {
            if counts[c] > 0 {
                centers[c] = sums[c].map(|s| s / counts[c] as f32);
            }
        }
    }
    let best = (0..K).max_by_key(|&c| counts[c])?;
    Some(centers[best])
}

fn luma(p: &[f32; 3]) -> f32 {
    0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]
}

fn nearest(centers: &[[f32; 3]], p: &[f32; 3]) -> usize {
    let dist = |c: &[f32; 3]| (0..3).map(|i| (c[i] - p[i]).powi(2)).sum::<f32>();
    (0..centers.len())
        .min_by(|&a, &b| dist(&centers[a]).total_cmp(&dist(&centers[b])))
        .unwrap_or(0)
}

fn linear(v: f32) -> f32 {
    let s = v / 255.0;
    if s <= 0.03928 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Contraste WCAG avec le blanc.
fn contrast_with_white(c: [f32; 3]) -> f32 {
    let l = 0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2]);
    1.05 / (l + 0.05)
}

/// Assombrit jusqu'à un contraste ≥ 4,5:1 avec le blanc.
fn readable(mut c: [f32; 3]) -> [u8; 3] {
    for _ in 0..60 {
        if contrast_with_white(c) >= MIN_CONTRAST {
            break;
        }
        c = c.map(|v| v * 0.95);
    }
    c.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn dominant_ignores_white_and_black() {
        let mut img = RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]));
        for x in 0..64 {
            for y in 0..20 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
            for y in 20..40 {
                img.put_pixel(x, y, Rgb([200, 40, 40]));
            }
        }
        let c = dominant(&img).unwrap();
        assert!(c[0] > 150.0 && c[1] < 80.0, "{c:?}");
    }

    #[test]
    fn blank_image_has_no_dominant_color() {
        assert!(dominant(&RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]))).is_none());
    }

    #[test]
    fn readable_reaches_contrast() {
        for c in [
            [255.0, 220.0, 0.0],
            [120.0, 200.0, 255.0],
            [10.0, 10.0, 90.0],
        ] {
            let out = readable(c).map(f32::from);
            assert!(contrast_with_white(out) >= MIN_CONTRAST - 0.05, "{out:?}");
        }
    }
}
