//! Screen geometry and frame comparison that hold on every OS: window placements, rectangle clipping and the block hashes the settle compares.

pub(crate) const BLOCK: usize = 16;
const HASH_MUL: u64 = 0x9e37_79b9_7f4a_7c15;
const RGB_BYTES: u64 = 0x00ff_ffff_00ff_ffff;

/// A mapped child of the root: a window frame, or a popup such as a menu.
#[derive(Clone, PartialEq)]
pub struct Placement {
    pub id: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    /// Override-redirect: a menu, popup or tooltip rather than a managed window.
    pub popup: bool,
    /// InputOutput: drawn on the screen, unlike an InputOnly window.
    pub drawn: bool,
}

impl Placement {
    pub(crate) fn contains(&self, x: i32, y: i32) -> bool {
        let (left, top) = (i32::from(self.x), i32::from(self.y));
        (left..left + i32::from(self.width)).contains(&x)
            && (top..top + i32::from(self.height)).contains(&y)
    }

    /// Whether the screen rectangle `[x, y, w, h]` lies inside this window.
    pub fn holds(&self, [x, y, w, h]: [i32; 4]) -> bool {
        self.contains(x, y) && self.contains(x + w - 1, y + h - 1)
    }
}

/// One hash per 16-pixel square of the BGRX rows `bgrx`, `width` pixels wide, so any changed pixel
/// changes its square's hash; the padding byte and the squares `skip` marks do not count.
pub fn block_hashes(bgrx: &[u8], width: usize, height: usize, skip: &[bool]) -> Vec<u64> {
    let columns = width.div_ceil(BLOCK);
    let mut hashes = vec![0u64; columns * height.div_ceil(BLOCK)];
    for y in 0..height {
        let base = y / BLOCK * columns;
        let row = &bgrx[y * width * 4..(y + 1) * width * 4];
        for (i, (h, piece)) in hashes[base..base + columns]
            .iter_mut()
            .zip(row.chunks(BLOCK * 4))
            .enumerate()
        {
            if skip.get(base + i) == Some(&true) {
                continue;
            }
            let (words, rest) = piece.as_chunks::<8>();
            for w in words {
                *h = mix(*h, u64::from_le_bytes(*w) & RGB_BYTES);
            }
            if let Some(p) = rest.first_chunk::<4>() {
                *h = mix(*h, u64::from(u32::from_le_bytes(*p)) & RGB_BYTES);
            }
        }
    }
    hashes
}

/// Whether a pixel of block `block` (see [`block_hashes`]) of the BGRX frames `a` and `b`, `width` by
/// `height`, differs by more than `step` in a colour channel; frames of different sizes always do.
pub fn visible(a: &[u8], b: &[u8], width: usize, height: usize, block: usize, step: u8) -> bool {
    if a.len() != b.len() || a.len() < width * height * 4 {
        return true;
    }
    let columns = width.div_ceil(BLOCK);
    let (x0, y0) = (block % columns * BLOCK, block / columns * BLOCK);
    let x1 = (x0 + BLOCK).min(width);
    (y0..(y0 + BLOCK).min(height)).any(|y| {
        let (start, end) = ((y * width + x0) * 4, (y * width + x1) * 4);
        a[start..end]
            .iter()
            .zip(&b[start..end])
            .enumerate()
            .any(|(k, (p, q))| k % 4 != 3 && p.abs_diff(*q) > step)
    })
}

/// The screen rectangle `[x, y, w, h]` clipped to a `width` by `height` screen in whole pixels, left and top down, right and bottom up; none when nothing is left.
pub fn clip([x, y, w, h]: [f64; 4], (width, height): (usize, usize)) -> Option<[i32; 4]> {
    let (width, height) = (width as f64, height as f64);
    let (left, top) = (x.max(0.0).floor(), y.max(0.0).floor());
    let (right, bottom) = ((x + w).min(width).ceil(), (y + h).min(height).ceil());
    (right > left && bottom > top)
        .then(|| [left, top, right - left, bottom - top].map(|v| v as i32))
}

fn mix(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(HASH_MUL).rotate_left(29)
}
