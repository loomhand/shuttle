//! Volatile screen blocks: the ones that change while no action runs, such as a clock, a caret,
//! a spinner or a video, learned from a low-rate MIT-SHM sampler that runs while tools are in use.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::xshm::Grabber;

const SAMPLE_EVERY: Duration = Duration::from_millis(200);
const WINDOW: Duration = Duration::from_secs(2);
const IDLE: Duration = Duration::from_secs(30);

/// What a screen block did in the samples before an action.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Block {
    #[default]
    Quiet,
    /// It changed at least twice between two contents, like a blinking caret; other contents are a change.
    Toggling(Vec<u64>),
    /// It changed at least twice through more contents, like a clock, a spinner or a video; no contents
    /// are a change. A block that changed once, like a window that opened, stays quiet.
    Churning,
}

impl Block {
    /// Whether the block showing `hash` after the action is a change the action may have made.
    pub fn counts(&self, hash: u64) -> bool {
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
        let mut seen = runs;
        seen.sort_unstable();
        seen.dedup();
        if seen.len() == 2 {
            Self::Toggling(seen)
        } else {
            Self::Churning
        }
    }

    fn merge(self, carried: &Self) -> Self {
        match (self, carried) {
            (Self::Churning, _) | (_, Self::Churning) => Self::Churning,
            (Self::Toggling(mut a), Self::Toggling(b)) => {
                a.extend(b);
                a.sort_unstable();
                a.dedup();
                Self::Toggling(a)
            }
            (Self::Quiet, c) => c.clone(),
            (b, Self::Quiet) => b,
        }
    }
}

/// The contents each block showed during a settle; a block back at its contents from before the
/// action toggles among them from then on, like a caret or a flicker the sampler did not catch.
#[derive(Default)]
pub struct Shown {
    states: HashMap<usize, Vec<u64>>,
}

impl Shown {
    /// Records the blocks that changed from `prev` to `now` and turns those back at `first` into toggling ones.
    pub fn learn(&mut self, first: &[u64], prev: &[u64], now: &[u64], blocks: &mut [Block]) {
        for (i, ((f, p), n)) in first.iter().zip(prev).zip(now).enumerate() {
            if p == n {
                continue;
            }
            let states = self.states.entry(i).or_insert_with(|| vec![*f]);
            if !states.contains(n) {
                states.push(*n);
            }
            if n == f
                && let Some(b) = blocks.get_mut(i)
            {
                *b = std::mem::take(b).merge(&Block::Toggling(states.clone()));
            }
        }
    }
}

/// The sampler's shared state; the sampler thread lives as long as the process.
pub struct Volatile {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    touched: Option<Instant>,
    /// Counts actions begun, so a sample grabbed across a begin is dropped.
    actions: u64,
    acting: bool,
    /// Block hashes sampled since the last action ended, oldest first.
    samples: VecDeque<(Instant, Vec<u64>)>,
    /// The blocks of the previous action, kept while the history is shorter than [`WINDOW`].
    carried: Vec<Block>,
}

impl Volatile {
    /// Starts the sampler thread, which samples only within [`IDLE`] of a [`Volatile::touch`].
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let sampler = Arc::clone(&shared);
        if let Err(e) = std::thread::Builder::new()
            .name("settle-sampler".to_owned())
            .spawn(move || sampler.run())
        {
            eprintln!("toolhost: settle sampler: {e}");
        }
        Self { shared }
    }

    /// Records a tool call, which keeps the sampler running for [`IDLE`].
    pub fn touch(&self) {
        self.shared.lock().touched = Some(Instant::now());
        self.shared.wake.notify_one();
    }

    /// Marks an action begun and returns what each block did over the samples of the last [`WINDOW`]
    /// and `now`, the hashes just before the action.
    pub fn begin(&self, now: &[u64]) -> Vec<Block> {
        let mut s = self.shared.lock();
        s.actions += 1;
        s.acting = true;
        let at = Instant::now();
        let mut recent: Vec<&[u64]> = s
            .samples
            .iter()
            .filter(|(t, _)| at.duration_since(*t) <= WINDOW)
            .map(|(_, h)| h.as_slice())
            .collect();
        recent.retain(|h| h.len() == now.len());
        recent.push(now);
        let mut blocks: Vec<Block> = (0..now.len())
            .map(|i| Block::of(recent.iter().map(|h| h[i])))
            .collect();
        let short = s
            .samples
            .front()
            .is_none_or(|(t, _)| at.duration_since(*t) + SAMPLE_EVERY < WINDOW);
        if short && s.carried.len() == blocks.len() {
            blocks = blocks
                .into_iter()
                .zip(&s.carried)
                .map(|(b, c)| b.merge(c))
                .collect();
        }
        s.carried.clone_from(&blocks);
        blocks
    }

    /// Marks the action ended; the samples taken across it are dropped.
    pub fn end(&self) {
        let mut s = self.shared.lock();
        s.acting = false;
        s.samples.clear();
        s.touched = Some(Instant::now());
        drop(s);
        self.shared.wake.notify_one();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&self) {
        let mut grabber: Option<Grabber> = None;
        let mut failing = false;
        loop {
            let actions = {
                let mut s = self.lock();
                while s.acting || s.touched.is_none_or(|t| t.elapsed() > IDLE) {
                    s = self
                        .wake
                        .wait_timeout(s, IDLE)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                s.actions
            };
            if grabber.is_none() {
                grabber = Grabber::open().ok();
            }
            match grabber.as_ref().map(Grabber::blocks) {
                Some(Ok(hashes)) => {
                    failing = false;
                    let mut s = self.lock();
                    if !s.acting && s.actions == actions {
                        let at = Instant::now();
                        s.samples.push_back((at, hashes));
                        while s
                            .samples
                            .front()
                            .is_some_and(|(t, _)| at.duration_since(*t) > WINDOW + SAMPLE_EVERY)
                        {
                            s.samples.pop_front();
                        }
                    }
                }
                Some(Err(e)) if !failing => {
                    eprintln!("toolhost: settle sampler: {e}");
                    (failing, grabber) = (true, None);
                }
                _ => grabber = None,
            }
            std::thread::sleep(SAMPLE_EVERY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_contents_toggle_more_churn_one_change_stays_quiet_and_all_carry_over() {
        let volatile = Volatile::start();
        let at = Instant::now();
        volatile.shared.lock().samples.extend([
            (at, vec![1, 1, 1, 1]),
            (at, vec![1, 2, 5, 1]),
            (at, vec![1, 1, 6, 3]),
        ]);
        let blocks = volatile.begin(&[1, 2, 7, 3]);
        assert_eq!(
            blocks,
            [
                Block::Quiet,
                Block::Toggling(vec![1, 2]),
                Block::Churning,
                Block::Quiet
            ]
        );
        assert!(blocks[0].counts(1) && !blocks[1].counts(2) && blocks[1].counts(3));
        assert!(!blocks[2].counts(9));
        volatile.end();
        assert!(volatile.shared.lock().samples.is_empty());
        assert_eq!(
            volatile.begin(&[1, 3, 7, 1])[1],
            Block::Toggling(vec![1, 2])
        );
        volatile.end();
        volatile
            .shared
            .lock()
            .samples
            .extend([(at - WINDOW, vec![1, 1, 1, 1]), (at, vec![5, 1, 1, 1])]);
        assert_eq!(volatile.begin(&[5, 1, 1, 1]), vec![Block::Quiet; 4]);
    }

    #[test]
    fn a_block_back_at_its_first_contents_toggles_among_what_it_showed() {
        let first = [1, 1, 1];
        let mut blocks = vec![Block::Quiet, Block::Quiet, Block::Churning];
        let mut shown = Shown::default();
        shown.learn(&first, &[1, 1, 1], &[2, 3, 4], &mut blocks);
        assert_eq!(blocks, [Block::Quiet, Block::Quiet, Block::Churning]);
        shown.learn(&first, &[2, 3, 4], &[5, 1, 1], &mut blocks);
        assert_eq!(
            blocks,
            [Block::Quiet, Block::Toggling(vec![1, 3]), Block::Churning]
        );
        assert!(!blocks[1].counts(3) && blocks[1].counts(6));
        assert!(blocks[0].counts(2));
    }
}
