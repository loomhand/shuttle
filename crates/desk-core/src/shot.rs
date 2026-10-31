//! Screenshot crops, with the windows of apps outside the allowlist painted over.

pub const OFF_SCREEN: &str = "the rectangle is off the screen";

const DEFLATE_LEVEL: u8 = 2;

/// A crop of the screen rectangle `[x, y, w, h]` from a screenshot scaled by `factor`, as `(width, height, PNG)`.
pub fn crop_png(
    png: &[u8],
    [x, y, w, h]: [f64; 4],
    factor: f64,
    mask: Option<&[([f64; 4], bool)]>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("screenshot too large")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let channels = info.color_type.samples();
    let clip = |v: f64, max: u32| (v * factor).floor().clamp(0.0, f64::from(max)) as u32;
    let (left, top) = (clip(x, info.width), clip(y, info.height));
    let (right, bottom) = (clip(x + w, info.width), clip(y + h, info.height));
    if right <= left || bottom <= top {
        return Err(OFF_SCREEN.to_owned());
    }
    let (cw, ch) = (right - left, bottom - top);
    let keep = mask.map(|m| keep_map(m, [left, top, right, bottom]));
    let mut rgb = Vec::with_capacity((cw * ch * 3) as usize);
    for row in top..bottom {
        for col in left..right {
            let i = ((row - top) * cw + col - left) as usize;
            if keep.as_ref().is_some_and(|k| !k[i]) {
                rgb.extend([0; 3]);
                continue;
            }
            let at = (row as usize * info.width as usize + col as usize) * channels;
            let px = &buf[at..at + channels];
            match channels {
                1 | 2 => rgb.extend([px[0]; 3]),
                _ => rgb.extend(&px[..3]),
            }
        }
    }
    Ok((cw, ch, encode(cw, ch, &rgb)?))
}

/// The RGB rows of the screen rectangle `[x, y, w, h]` as a PNG, with the windows outside `mask`'s admitted ones painted black.
pub fn rgb_png(
    mut rgb: Vec<u8>,
    [x, y, w, h]: [u32; 4],
    mask: Option<&[([f64; 4], bool)]>,
) -> Result<Vec<u8>, String> {
    if let Some(m) = mask {
        let keep = keep_map(m, [x, y, x + w, y + h]);
        for (px, kept) in rgb.chunks_mut(3).zip(keep) {
            if !kept {
                px.fill(0);
            }
        }
    }
    encode(w, h, &rgb)
}

fn encode(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_deflate_compression(png::DeflateCompression::Level(DEFLATE_LEVEL));
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(rgb))
        .map_err(|e| format!("encode PNG: {e}"))?;
    Ok(out)
}

fn keep_map(mask: &[([f64; 4], bool)], [left, top, right, bottom]: [u32; 4]) -> Vec<bool> {
    let cw = (right - left) as usize;
    let mut keep = vec![false; cw * (bottom - top) as usize];
    for &([x, y, w, h], admitted) in mask {
        let span = |a: f64, b: f64, min: u32, max: u32| {
            let (a, b) = if admitted {
                (a.ceil(), b.floor())
            } else {
                (a.floor(), b.ceil())
            };
            let clip = |v: f64| (v.clamp(f64::from(min), f64::from(max)) as u32 - min) as usize;
            clip(a)..clip(b)
        };
        let cols = span(x, x + w, left, right);
        for row in span(y, y + h, top, bottom) {
            keep[row * cw + cols.start..row * cw + cols.end.max(cols.start)].fill(admitted);
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crop_scales_screen_pixels_to_the_screenshot() {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 4, 2);
        enc.set_color(png::ColorType::Rgba);
        let mut px = Vec::new();
        for i in 0..8u8 {
            px.extend([i, i, i, 255]);
        }
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        let (w, h, _) = crop_png(&png, [2.0, 0.0, 4.0, 4.0], 0.5, None).unwrap();
        assert_eq!((w, h), (2, 2));
        let (w, h, _) =
            crop_png(&png, [0.0, 0.0, f64::MAX / 4.0, f64::MAX / 4.0], 1.0, None).unwrap();
        assert_eq!((w, h), (4, 2));
        assert!(crop_png(&png, [100.0, 0.0, 4.0, 4.0], 1.0, None).is_err());
    }

    #[test]
    fn crop_cuts_an_rgb_rectangle() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| i as u8).collect();
        let mut src = Vec::new();
        let mut enc = png::Encoder::new(&mut src, 3, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        let (w, h, out) = crop_png(&src, [1.0, 1.0, 5.0, 5.0], 1.0, None).unwrap();
        assert_eq!((w, h), (2, 1));
        let mut dec = png::Decoder::new(std::io::Cursor::new(out))
            .read_info()
            .unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..6], &[16, 17, 18, 20, 21, 22]);
    }

    #[test]
    fn a_screenshot_png_decodes_to_the_pixels_it_was_encoded_from() {
        let rgb: Vec<u8> = (0..64 * 48 * 3u32).map(|i| (i * i / 7) as u8).collect();
        let png = rgb_png(rgb.clone(), [0, 0, 64, 48], None).unwrap();
        let mut dec = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert_eq!(buf, rgb);
    }

    #[test]
    fn a_backend_capture_paints_the_unadmitted_windows_black() {
        let rgb = vec![200; 4 * 2 * 3];
        let mask = [([0.0, 0.0, 4.0, 2.0], false), ([2.0, 0.0, 2.0, 2.0], true)];
        let png = rgb_png(rgb, [0, 0, 4, 2], Some(&mask)).unwrap();
        let mut dec = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..6], &[0; 6]);
        assert_eq!(&buf[6..12], &[200; 6]);
    }
}
