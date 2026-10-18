//! The screen-still wait's block model, ported from cocoonbox's settle: a block that keeps changing during the wait, like a clock, a spinner or a video, stops counting, a block toggling between two contents, like a caret, counts only with new contents, and a faint change does not count.

const BLOCK: usize = 8;
const VISIBLE_STEP: u8 = 24;

/// What a screen block did during the wait.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Block {
    #[default]
    Quiet,
    /// It changed at least twice between two contents; other contents are a change.
    Toggling(Vec<u64>),
    /// It changed at least twice through more contents; no contents are a change.
    Churning,
}

impl Block {
    /// Whether the block showing `hash` is a change that counts.
    pub(crate) fn counts(&self, hash: u64) -> bool {
        match self {
            Self::Quiet => true,
            Self::Toggling(seen) => !seen.contains(&hash),
            Self::Churning => false,
        }
    }

    fn of(contents: impl Iterator<Item = u64>) -> Self {
        let mut runs: Vec<u64> = Vec::new();
        for h in contents {
            if runs.last() != Some(&h) {
                runs.push(h);
            }
        }
        if runs.len() < 3 {
            return Self::Quiet;
        }
        runs.sort_unstable();
        runs.dedup();
        if runs.len() == 2 {
            Self::Toggling(runs)
        } else {
            Self::Churning
        }
    }
}

/// One capture: its RGB pixels and a hash per block.
pub(crate) struct Frame {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    hashes: Vec<u64>,
}

impl Frame {
    /// Decodes a PNG capture.
    pub(crate) fn decode(png: &[u8]) -> Result<Self, String> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(png));
        dec.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = dec
            .read_info()
            .map_err(|e| format!("decode capture: {e}"))?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("capture too large")?];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| format!("decode capture: {e}"))?;
        let channels = info.color_type.samples();
        let rgb: Vec<u8> = buf[..info.buffer_size()]
            .chunks(channels)
            .flat_map(|p| {
                if channels < 3 {
                    [p[0]; 3]
                } else {
                    [p[0], p[1], p[2]]
                }
            })
            .collect();
        Ok(Self::of_rgb(info.width as usize, info.height as usize, rgb))
    }

    fn of_rgb(width: usize, height: usize, rgb: Vec<u8>) -> Self {
        let columns = width.div_ceil(BLOCK);
        let mut hashes = vec![0xcbf2_9ce4_8422_2325u64; columns * height.div_ceil(BLOCK)];
        for (y, row) in rgb.chunks(width * 3).enumerate() {
            for (x, px) in row.chunks(3).enumerate() {
                let h = &mut hashes[y / BLOCK * columns + x / BLOCK];
                for b in px {
                    *h = (*h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        Self {
            width,
            height,
            rgb,
            hashes,
        }
    }

    fn visible(&self, other: &Self, block: usize) -> bool {
        let columns = self.width.div_ceil(BLOCK);
        let (bx, by) = (block % columns * BLOCK, block / columns * BLOCK);
        (by..(by + BLOCK).min(self.height)).any(|y| {
            let row =
                (y * self.width + bx) * 3..(y * self.width + (bx + BLOCK).min(self.width)) * 3;
            self.rgb[row.clone()]
                .iter()
                .zip(&other.rgb[row])
                .any(|(a, b)| a.abs_diff(*b) > VISIBLE_STEP)
        })
    }
}

/// The contents each block showed during one wait.
#[derive(Default)]
pub(crate) struct Watch {
    runs: Vec<Vec<u64>>,
}

impl Watch {
    /// Records `now` after `last` and says whether a block changed in a way that counts: visibly, and not a block that churns or toggled back to contents it showed.
    pub(crate) fn counts(&mut self, last: &Frame, now: &Frame) -> bool {
        if (last.width, last.height) != (now.width, now.height) {
            self.runs.clear();
            return true;
        }
        if self.runs.is_empty() {
            self.runs = last.hashes.iter().map(|h| vec![*h]).collect();
        }
        let mut counted = false;
        for (i, (was, is)) in last.hashes.iter().zip(&now.hashes).enumerate() {
            if was == is {
                continue;
            }
            let runs = &mut self.runs[i];
            if Block::of(runs.iter().copied()).counts(*is) && last.visible(now, i) {
                counted = true;
            }
            runs.push(*is);
        }
        counted
    }

    /// How many blocks churned or toggled during the wait.
    pub(crate) fn volatile(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| Block::of(r.iter().copied()) != Block::Quiet)
            .count()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `width` x `height` frame, black but for the 8-pixel blocks in `lit` painted `tone`.
    pub(crate) fn frame(width: usize, height: usize, lit: &[(usize, u8)]) -> Frame {
        let columns = width.div_ceil(BLOCK);
        let mut rgb = vec![0; width * height * 3];
        for (y, row) in rgb.chunks_mut(width * 3).enumerate() {
            for (x, px) in row.chunks_mut(3).enumerate() {
                if let Some((_, tone)) = lit
                    .iter()
                    .find(|(b, _)| *b == y / BLOCK * columns + x / BLOCK)
                {
                    px.fill(*tone);
                }
            }
        }
        Frame::of_rgb(width, height, rgb)
    }

    #[test]
    fn two_contents_toggle_more_churn_and_one_change_stays_quiet() {
        assert_eq!(Block::of([1, 1, 2, 2].into_iter()), Block::Quiet);
        assert_eq!(
            Block::of([1, 2, 1].into_iter()),
            Block::Toggling(vec![1, 2])
        );
        assert_eq!(Block::of([1, 2, 3].into_iter()), Block::Churning);
        assert!(Block::Quiet.counts(5) && !Block::Churning.counts(5));
        assert!(!Block::Toggling(vec![1, 2]).counts(2) && Block::Toggling(vec![1, 2]).counts(3));
    }

    #[test]
    fn a_churning_block_and_a_caret_stop_counting_but_a_new_change_and_a_resize_count() {
        let mut w = Watch::default();
        let video = |tone: u8| frame(32, 16, &[(0, tone)]);
        assert!(w.counts(&video(10), &video(60)));
        assert!(w.counts(&video(60), &video(110)));
        assert!(
            !w.counts(&video(110), &video(160)),
            "the third content churns"
        );
        let mut caret = Watch::default();
        let blink = |on: bool| frame(32, 16, &[(1, if on { 200 } else { 0 })]);
        assert!(caret.counts(&blink(false), &blink(true)));
        assert!(caret.counts(&blink(true), &blink(false)));
        assert!(
            !caret.counts(&blink(false), &blink(true)),
            "back at a shown content"
        );
        assert!(caret.counts(&blink(true), &frame(32, 16, &[(1, 90)])));
        assert_eq!(caret.volatile(), 1);
        let mut faint = Watch::default();
        assert!(!faint.counts(&frame(32, 16, &[(2, 100)]), &frame(32, 16, &[(2, 110)])));
        assert!(Watch::default().counts(&frame(32, 16, &[]), &frame(16, 16, &[])));
    }
}
