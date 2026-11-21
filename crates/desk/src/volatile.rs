//! Volatile screen blocks, such as a clock, a caret or a spinner, learned from a low-rate MIT-SHM sampler.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::still::Block;
use crate::xshm::Grabber;

const SAMPLE_EVERY: Duration = Duration::from_millis(200);
const WINDOW: Duration = Duration::from_secs(2);
const IDLE: Duration = Duration::from_secs(30);
const CHURN_STATES: usize = 4;

/// The contents each block showed during a settle; a block back at its contents from before the action toggles among them from then on, like a caret or a flicker the sampler did not catch.
#[derive(Default)]
pub struct Shown {
    states: HashMap<usize, Vec<u64>>,
}

impl Shown {
    /// Records the blocks that changed from `prev` to `now`, turns those back at `first` into toggling ones, and says whether one showed its fourth contents.
    pub fn learn(
        &mut self,
        first: &[u64],
        prev: &[u64],
        now: &[u64],
        blocks: &mut [Block],
    ) -> bool {
        let mut churned = false;
        for (i, ((f, p), n)) in first.iter().zip(prev).zip(now).enumerate() {
            if p == n || self.churns(i) {
                continue;
            }
            let Some(b) = blocks.get_mut(i).filter(|b| **b != Block::Churning) else {
                continue;
            };
            let states = self.states.entry(i).or_insert_with(|| vec![*f]);
            if !states.contains(n) {
                states.push(*n);
            }
            if states.len() >= CHURN_STATES {
                churned = true;
            } else if n == f {
                *b = merge(std::mem::take(b), Block::Toggling(states.clone()));
            }
        }
        churned
    }

    /// Whether block `i` showed four contents during this settle: its changes still move the screen, but no longer differ from before the action.
    pub fn churns(&self, i: usize) -> bool {
        self.states.get(&i).is_some_and(|s| s.len() >= CHURN_STATES)
    }
}

/// The sampler's shared state; the sampler thread lives as long as the process.
pub struct Volatile {
    shared: Arc<Shared>,
}

impl Volatile {
    /// Starts the sampler thread where MIT-SHM exists, which samples only within [`IDLE`] of a [`Volatile::touch`].
    pub fn start(turn: Arc<tokio::sync::Mutex<()>>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            turn,
        });
        let sampler = Arc::clone(&shared);
        if cfg!(target_os = "linux")
            && let Err(e) = std::thread::Builder::new()
                .name("settle-sampler".to_owned())
                .spawn(move || sampler.run())
        {
            eprintln!("desk: settle sampler: {e}");
        }
        Self { shared }
    }

    /// Records a tool call, which keeps the sampler running for [`IDLE`].
    pub fn touch(&self) {
        self.shared.lock().touched = Some(Instant::now());
        self.shared.wake.notify_one();
    }

    /// Marks an action begun until the returned [`Paused`] drops; returns what each block did in the last [`WINDOW`] of samples.
    pub fn begin(&self, now: &[u64]) -> (Vec<Block>, Paused) {
        let mut s = self.shared.lock();
        s.actions += 1;
        s.acting = true;
        let at = Instant::now();
        let mut pause: Vec<(Instant, Vec<u64>)> = std::mem::take(&mut s.samples)
            .into_iter()
            .filter(|(t, h)| at.duration_since(*t) <= WINDOW && h.len() == now.len())
            .collect();
        pause.push((at, now.to_vec()));
        s.pauses.retain(|p| p[0].1.len() == now.len());
        if pause.len() >= 3 {
            s.pauses.push_front(pause);
        }
        let mut span = Duration::ZERO;
        let kept = s
            .pauses
            .iter()
            .take_while(|p| {
                let newer = span;
                span += p[p.len() - 1].0.duration_since(p[0].0);
                newer + SAMPLE_EVERY < WINDOW
            })
            .count();
        s.pauses.truncate(kept);
        let blocks = (0..now.len())
            .map(|i| {
                s.pauses.iter().fold(Block::Quiet, |carried, p| {
                    merge(Block::of(p.iter().map(|(_, h)| h[i])), carried)
                })
            })
            .collect();
        (blocks, Paused(Arc::clone(&self.shared)))
    }
}

pub struct Paused(Arc<Shared>);

impl Drop for Paused {
    fn drop(&mut self) {
        let mut s = self.0.lock();
        s.acting = false;
        s.samples.clear();
        s.touched = Some(Instant::now());
        drop(s);
        self.0.wake.notify_one();
    }
}

#[derive(Default)]
struct State {
    touched: Option<Instant>,
    /// Counts actions begun, so a sample grabbed across a begin is dropped.
    actions: u64,
    acting: bool,
    held: bool,
    /// Block hashes sampled since the last action ended, oldest first.
    samples: VecDeque<(Instant, Vec<u64>)>,
    /// The samples of the pauses before earlier actions, newest first, kept while the newer ones span less than [`WINDOW`] by more than [`SAMPLE_EVERY`].
    pauses: VecDeque<Vec<(Instant, Vec<u64>)>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    turn: Arc<tokio::sync::Mutex<()>>,
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
            grabber = grabber
                .take()
                .filter(Grabber::current)
                .or_else(|| Grabber::open().ok());
            match grabber.as_ref().map(Grabber::blocks) {
                Some(Ok(hashes)) => {
                    failing = false;
                    self.record(actions, hashes);
                }
                Some(Err(e)) if !failing => {
                    eprintln!("desk: settle sampler: {e}");
                    (failing, grabber) = (true, None);
                }
                _ => grabber = None,
            }
            std::thread::sleep(SAMPLE_EVERY);
        }
    }

    fn record(&self, actions: u64, hashes: Vec<u64>) {
        let mut s = self.lock();
        if self.turn.try_lock().is_err() {
            s.held = true;
        } else if !s.acting && s.actions == actions {
            if std::mem::take(&mut s.held) {
                s.samples.clear();
            }
            let at = Instant::now();
            s.samples.push_back((at, hashes));
            s.samples
                .retain(|(t, _)| at.duration_since(*t) <= WINDOW + SAMPLE_EVERY);
        }
    }
}

/// What `block` did in one pause together with what it did in the later pauses, `carried`.
fn merge(block: Block, carried: Block) -> Block {
    match (block, carried) {
        (Block::Churning, _) | (_, Block::Churning) => Block::Churning,
        (Block::Toggling(mut a), Block::Toggling(b)) => {
            a.extend(b);
            a.sort_unstable();
            a.dedup();
            Block::Toggling(a)
        }
        (Block::Quiet, c) => c,
        (b, Block::Quiet) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_contents_toggle_more_churn_one_stays_quiet_and_all_last_while_newer_pauses_are_short() {
        let volatile = Volatile::start(Arc::default());
        let at = Instant::now();
        volatile.shared.lock().samples.extend([
            (at, vec![1, 1, 1, 1]),
            (at, vec![1, 2, 5, 1]),
            (at, vec![1, 1, 6, 3]),
        ]);
        let (blocks, paused) = volatile.begin(&[1, 2, 7, 3]);
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
        drop(paused);
        assert!(volatile.shared.lock().samples.is_empty());
        let (blocks, mut paused) = volatile.begin(&[1, 3, 7, 1]);
        assert_eq!(blocks[1], Block::Toggling(vec![1, 2]));
        for kept in [true, false] {
            drop(paused);
            let t = Instant::now();
            volatile.shared.lock().samples.extend([
                (t - WINDOW * 3 / 4, vec![1, 3, 7, 1]),
                (t - WINDOW / 2, vec![1, 3, 7, 1]),
            ]);
            let blocks;
            (blocks, paused) = volatile.begin(&[1, 3, 7, 1]);
            assert_eq!(blocks[2] == Block::Churning, kept);
            assert_eq!(blocks.iter().all(|b| *b == Block::Quiet), !kept);
        }
    }

    #[test]
    fn samples_taken_while_another_input_holds_the_turn_drop_with_the_ones_before() {
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        let volatile = Volatile::start(Arc::clone(&turn));
        for h in [1, 2, 1] {
            volatile.shared.record(0, vec![h]);
        }
        let held = turn.try_lock().unwrap();
        for h in [3, 4] {
            volatile.shared.record(0, vec![h]);
        }
        drop(held);
        for h in [5, 5] {
            volatile.shared.record(0, vec![h]);
        }
        let (blocks, _paused) = volatile.begin(&[5]);
        assert_eq!(blocks, [Block::Quiet]);
    }

    #[test]
    fn an_action_dropped_before_its_settle_lets_the_sampler_run_again() {
        let volatile = Volatile::start(Arc::default());
        let (_, paused) = volatile.begin(&[1]);
        assert!(volatile.shared.lock().acting);
        drop(paused);
        assert!(!volatile.shared.lock().acting);
    }

    #[test]
    fn a_block_that_shows_a_fourth_contents_during_one_wait_churns_once() {
        let first = [1, 1];
        let mut blocks = vec![Block::Quiet, Block::Quiet];
        let mut shown = Shown::default();
        assert!(!shown.learn(&first, &[1, 1], &[2, 2], &mut blocks));
        assert!(!shown.learn(&first, &[2, 2], &[3, 1], &mut blocks));
        assert!(shown.learn(&first, &[3, 1], &[4, 2], &mut blocks));
        assert_eq!(blocks, [Block::Quiet, Block::Toggling(vec![1, 2])]);
        assert!(!shown.learn(&first, &[4, 2], &[5, 2], &mut blocks));
        assert!(shown.churns(0) && !shown.churns(1));
        assert!(blocks[0].counts(6));
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
