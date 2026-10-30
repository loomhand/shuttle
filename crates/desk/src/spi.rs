//! The Linux desk backend: the AT-SPI client's async calls, run on the worker thread by a runtime of its own so their fan-out stays concurrent, and screen reads over MIT-SHM.

use std::time::Duration;

use desk_core::window::{TITLE_TIERS, title_matches};
use tokio::runtime::{Builder, Runtime};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask};

use crate::atspi::{self, Atspi, Obj};
use crate::model::{Access, Act, Capture, Extras, Frame, Modal, Node, Tree, focused_by_tree};
use crate::xshm::{Grabber, clip};

const FRAME_ROLES: [u32; 5] = [2, 16, 19, 23, WINDOW_ROLE];
const WINDOW_ROLE: u32 = 69;

/// The AT-SPI client, the single-thread runtime its calls run on, and the MIT-SHM grabber once opened.
pub struct Spi {
    rt: Runtime,
    atspi: Atspi,
    grabber: Option<Grabber>,
}

impl Spi {
    pub fn open() -> Result<Self, String> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("start the AT-SPI runtime: {e}"))?;
        Ok(Self {
            rt,
            atspi: Atspi::new(),
            grabber: None,
        })
    }

    /// `read` on the grabber; none when MIT-SHM is out of reach.
    fn shm<T>(&mut self, read: impl Fn(&Grabber) -> Result<T, String>) -> Option<T> {
        reuse(&mut self.grabber, Grabber::open, Grabber::current, read)
    }

    fn listed(&self) -> Result<Vec<atspi::Frame>, String> {
        let listed = self.rt.block_on(self.atspi.frames());
        if listed.is_err() {
            self.rt.block_on(self.atspi.reset());
        }
        listed
    }
}

impl Access for Spi {
    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        let mut frames: Vec<Frame> = self.listed()?.into_iter().map(frame).collect();
        if let Some(Some((pid, title))) =
            self.shm(|g| Ok(g.active().and_then(|w| Some((g.pid(w), g.title(w)?)))))
        {
            settle_active(&mut frames, pid, &title);
        }
        Ok(frames)
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let window = atspi::Frame {
            obj: Obj::parse(&frame.id)?,
            role: 0,
            name: frame.title.clone(),
            states: 0,
            pid: frame.pid,
        };
        let t = self
            .rt
            .block_on(self.atspi.tree(window, spi_extras(extras)))?;
        Ok(Tree {
            frame: frame.clone(),
            nodes: t.nodes.into_iter().map(node).collect(),
            truncated: t.truncated,
            phases: t.phases,
        })
    }

    fn focused(&mut self, frame: &Frame, within: Duration) -> Result<Option<String>, String> {
        let obj = Obj::parse(&frame.id)?;
        match answered(&self.rt, within, self.atspi.focused_in(&obj)) {
            None => Ok(None),
            Some(Some(found)) => Ok(Some(found.id())),
            Some(None) => focused_by_tree(self, frame),
        }
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let obj = Obj::parse(id)?;
        let n = self
            .rt
            .block_on(self.atspi.node(&obj, spi_extras(extras), chars))?;
        Ok(node(n))
    }

    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
        let obj = Obj::parse(id)?;
        let chain = self.rt.block_on(self.atspi.ancestors(&obj))?;
        Ok(chain.iter().map(Obj::id).collect())
    }

    fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String> {
        let within = Obj::parse(window)?;
        let hit = self.rt.block_on(self.atspi.at_point(&within, x, y))?;
        Ok(hit.as_ref().map(Obj::id))
    }

    fn covering(
        &mut self,
        frame: &Frame,
        x: i32,
        y: i32,
    ) -> Result<Option<(String, Option<u32>)>, String> {
        let Some(top) = self.shm(|g| g.client_at(x, y)).flatten() else {
            return Ok(None);
        };
        if top.1 != frame.pid {
            return Ok(Some(top));
        }
        let titles = self.shm(|g| g.titles(top.1)).unwrap_or_default();
        Ok((!is_frame(frame, &top, &titles)).then_some(top))
    }

    fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
        let obj = Obj::parse(id)?;
        Ok(self.rt.block_on(self.atspi.pid(&obj.bus)).ok())
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let obj = Obj::parse(id)?;
        self.rt.block_on(async {
            match act {
                Act::Action => self.atspi.do_action(&obj).await,
                Act::Select => self.atspi.select(&obj).await,
                Act::Focus => self.atspi.grab_focus(&obj).await,
            }
        })
    }

    fn capture(&mut self, rect: [f64; 4], overlay: bool) -> Result<Option<Capture>, String> {
        let Some((size, shown)) = self.shm(|g| Ok((g.size(), overlay && g.overlay_shown()?)))
        else {
            return Ok(None);
        };
        if shown {
            return Ok(None);
        }
        let clipped = clip(rect, size).ok_or("the rectangle is off the screen")?;
        let rgb = self.shm(|g| g.region(clipped));
        Ok(rgb.map(|rgb| Capture {
            rect: clipped.map(i32::unsigned_abs),
            rgb,
        }))
    }

    fn modals(&mut self) -> Vec<Modal> {
        self.shm(Grabber::modals).unwrap_or_default()
    }

    fn close(&mut self, frame: &Frame) -> Result<(), String> {
        close(&frame.title, frame.pid)
    }

    fn shells(&mut self) -> Vec<u32> {
        self.shm(|g| Ok(g.shell_pids())).unwrap_or_default()
    }

    fn unanswered(&mut self) -> Option<Frame> {
        let (pid, title) =
            self.shm(|g| Ok(g.active().and_then(|w| Some((g.pid(w)?, g.title(w)?)))))??;
        let frames = self.listed().ok()?;
        unlisted(frames.iter().map(|f| f.pid), pid).then(|| Frame {
            title,
            role: "window",
            pid: Some(pid),
            app: app(pid),
            ..Frame::default()
        })
    }
}

fn frame(f: atspi::Frame) -> Frame {
    Frame {
        id: f.obj.id(),
        role: atspi::role_name(f.role, true),
        pid: f.pid,
        active: f.active(),
        app: f.pid.and_then(app),
        owner: None,
        modal: None,
        blocks: None,
        shell: false,
        space: None,
        stale: false,
        unlisted: !FRAME_ROLES.contains(&f.role),
        framed: f.role != WINDOW_ROLE,
        hidden: false,
        number: None,
        title: f.name,
    }
}

/// Leaves active only the frame of the X active window, `_NET_ACTIVE_WINDOW`'s pid and title, since GTK keeps a window's AT-SPI active state after the keyboard focus moved to another X window; title twins of that pid leave none active, a title of no frame (or one that changed between the AT-SPI and X reads) keeps that pid's AT-SPI actives, and a pid of no frame keeps AT-SPI's state.
fn settle_active(frames: &mut [Frame], pid: Option<u32>, title: &str) {
    let Some(pid) = pid.filter(|&p| p != 0 && frames.iter().any(|f| f.pid == Some(p))) else {
        return;
    };
    let named = if title.is_empty() {
        0
    } else {
        frames
            .iter()
            .filter(|f| f.pid == Some(pid) && f.title == title)
            .count()
    };
    for f in frames.iter_mut() {
        let own = f.pid == Some(pid);
        f.active = match named {
            0 => f.active && own,
            1 => own && f.title == title,
            _ => false,
        };
    }
}

/// Whether process `pid` owns none of the frames whose processes are `pids`.
fn unlisted(mut pids: impl Iterator<Item = Option<u32>>, pid: u32) -> bool {
    pid != 0 && pids.all(|p| p != Some(pid))
}

/// Whether the X window `(title, pid)` is `frame`'s own: of its process, and that process's only X window or the one with that title, `titles` being the titles of the process's X windows.
fn is_frame(frame: &Frame, (title, pid): &(String, Option<u32>), titles: &[String]) -> bool {
    *pid == frame.pid
        && (titles.len() == 1
            || (*title == frame.title && titles.iter().filter(|t| *t == title).count() == 1))
}

fn node(n: atspi::Node) -> Node {
    let (mnemonic, shortcut) = n.keys.as_deref().map_or((None, None), atspi::key_combos);
    Node {
        id: n.obj.as_ref().map(Obj::id).unwrap_or_default(),
        parent: n.parent,
        depth: n.depth,
        role: n.role(),
        native_role: n.native_role().map(str::to_owned),
        states: n.state_names(),
        shortcut,
        mnemonic,
        bounds: n.bounds,
        name: n.name,
        description: n.description,
        value: n.value,
        keys: n.keys,
    }
}

/// Asks the window manager to close the managed window titled `want` of process `pid`, as its close button does; refuses a title two windows share and a window manager without `_NET_CLOSE_WINDOW`.
fn close(want: &str, pid: Option<u32>) -> Result<(), String> {
    let (conn, n) = x11rb::connect(None).map_err(|e| format!("connect X: {e}"))?;
    let root = conn.setup().roots[n].root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        conn.intern_atom(false, name)
            .map_err(|e| format!("intern atom: {e}"))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| format!("intern atom: {e}"))
    };
    let [supported, list, name, utf8, wm_pid, close] = [
        &b"_NET_SUPPORTED"[..],
        b"_NET_CLIENT_LIST",
        b"_NET_WM_NAME",
        b"UTF8_STRING",
        b"_NET_WM_PID",
        b"_NET_CLOSE_WINDOW",
    ]
    .map(atom);
    let (supported, list, name, utf8, wm_pid, close) =
        (supported?, list?, name?, utf8?, wm_pid?, close?);
    let prop = |w: u32, p: u32, kind: u32| {
        conn.get_property(false, w, p, kind, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
    };
    let closes = prop(root, supported, AtomEnum::ATOM.into())
        .and_then(|r| r.value32().map(|mut v| v.any(|a| a == close)))
        .unwrap_or(false);
    if !closes {
        return Err(format!(
            "close {want:?}: the window manager does not support _NET_CLOSE_WINDOW"
        ));
    }
    let clients: Vec<u32> = prop(root, list, AtomEnum::WINDOW.into())
        .and_then(|r| r.value32().map(Iterator::collect))
        .unwrap_or_default();
    let windows: Vec<(u32, String)> = clients
        .into_iter()
        .filter(|&w| {
            pid.is_none_or(|p| {
                prop(w, wm_pid, AtomEnum::CARDINAL.into())
                    .and_then(|r| r.value32().and_then(|mut v| v.next()))
                    == Some(p)
            })
        })
        .map(|w| {
            let title = prop(w, name, utf8)
                .map(|r| String::from_utf8_lossy(&r.value).into_owned())
                .unwrap_or_default();
            (w, title)
        })
        .collect();
    let w = pick_window(&windows, want)?;
    let event = ClientMessageEvent::new(32, w, close, [0, 2, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|e| format!("close {want:?}: {e}"))?;
    // a round trip, not a flush: a connection that ends right after a flush can lose the event
    conn.get_input_focus()
        .map_err(|e| format!("close {want:?}: {e}"))?
        .reply()
        .map(drop)
        .map_err(|e| format!("close {want:?}: {e}"))
}

/// The one window of `windows` whose title matches `want` at the closest tier; two at that tier are refused.
fn pick_window(windows: &[(u32, String)], want: &str) -> Result<u32, String> {
    let found = (0..TITLE_TIERS)
        .map(|tier| {
            windows
                .iter()
                .filter(|(_, t)| title_matches(tier, t, want))
                .collect::<Vec<_>>()
        })
        .find(|m| !m.is_empty())
        .ok_or_else(|| format!("no window {want:?}"))?;
    match found[..] {
        [(w, _)] => Ok(*w),
        _ => Err(format!(
            "close {want:?}: {} windows of the process have that title; close one with its close button",
            found.len()
        )),
    }
}

/// The file name of process `pid`'s executable, as the allowlist names a Linux app; none when it cannot be read, which the allowlist refuses.
fn app(pid: u32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = exe.file_name()?.to_string_lossy();
    Some(name.trim_end_matches(" (deleted)").to_owned()).filter(|n| !n.is_empty())
}

fn spi_extras(e: Extras) -> atspi::Extras {
    atspi::Extras {
        values: e.values,
        bounds: e.bounds,
        keys: e.keys,
        frame: e.frame,
    }
}

/// What `lookup` answers within `within` on `rt`; none once the time is up, which drops the lookup and frees the runtime.
fn answered<T>(
    rt: &tokio::runtime::Runtime,
    within: Duration,
    lookup: impl Future<Output = T>,
) -> Option<T> {
    rt.block_on(async { tokio::time::timeout(within, lookup).await })
        .ok()
}

/// `read` on the grabber in `slot`, opened on first use and again after a screen change; a failed read opens it once more and retries, and a second failure leaves the slot empty.
fn reuse<G, T>(
    slot: &mut Option<G>,
    open: impl Fn() -> Result<G, String>,
    current: impl Fn(&G) -> bool,
    read: impl Fn(&G) -> Result<T, String>,
) -> Option<T> {
    let g = match slot.take() {
        Some(g) if current(&g) => g,
        stale => {
            drop(stale);
            open().ok()?
        }
    };
    let (g, out) = match read(&g) {
        Ok(out) => (g, out),
        Err(_) => {
            drop(g);
            let g = open().ok()?;
            let out = read(&g).ok()?;
            (g, out)
        }
    };
    *slot = Some(g);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(title: &str, pid: u32, active: bool) -> Frame {
        Frame {
            title: title.to_owned(),
            pid: Some(pid),
            active,
            ..Frame::default()
        }
    }

    fn actives(frames: &[Frame]) -> Vec<&str> {
        frames
            .iter()
            .filter(|f| f.active)
            .map(|f| f.title.as_str())
            .collect()
    }

    #[test]
    fn the_x_active_window_wins_over_a_stale_at_spi_active_state() {
        let mut frames = vec![
            window("a.txt - Mousepad", 7, true),
            window("b.txt - Mousepad", 7, false),
            window("Terminal", 9, false),
        ];
        settle_active(&mut frames, Some(7), "b.txt - Mousepad");
        assert_eq!(actives(&frames), ["b.txt - Mousepad"]);
        let mut frames = vec![
            window("a.txt - Mousepad", 7, true),
            window("Terminal", 9, false),
        ];
        settle_active(&mut frames, Some(9), "Terminal");
        assert_eq!(actives(&frames), ["Terminal"]);
    }

    #[test]
    fn an_x_active_window_without_a_frame_of_its_title_keeps_only_its_pids_at_spi_actives() {
        let mut frames = vec![
            window("a.txt - Mousepad", 7, true),
            window("Terminal", 9, true),
        ];
        settle_active(&mut frames, Some(9), "Desktop");
        assert_eq!(actives(&frames), ["Terminal"]);
        let mut frames = vec![window("a.txt - Mousepad", 7, true)];
        settle_active(&mut frames, None, "a.txt - Mousepad");
        assert_eq!(actives(&frames), ["a.txt - Mousepad"]);
    }

    #[test]
    fn title_twins_of_the_x_active_window_leave_no_frame_active() {
        for marked in [[false, false], [false, true], [true, true]] {
            let mut frames = vec![
                window("Untitled", 7, marked[0]),
                window("Untitled", 7, marked[1]),
                window("Terminal", 9, true),
            ];
            settle_active(&mut frames, Some(7), "Untitled");
            assert!(actives(&frames).is_empty(), "{marked:?}");
        }
    }

    #[test]
    fn an_untitled_x_active_window_never_names_an_untitled_popup_active() {
        let mut frames = vec![
            window("a.txt - Mousepad", 7, true),
            window("", 7, false),
            window("Terminal", 9, true),
        ];
        settle_active(&mut frames, Some(7), "");
        assert_eq!(actives(&frames), ["a.txt - Mousepad"]);
    }

    #[test]
    fn only_another_x_window_covers_a_frame() {
        let mousepad = window("a.txt - Mousepad", 7, true);
        let titles = |t: &[&str]| t.iter().map(|&x| x.to_owned()).collect::<Vec<_>>();
        let own = ("a.txt - Mousepad".to_owned(), Some(7));
        assert!(is_frame(
            &mousepad,
            &own,
            &titles(&["a.txt - Mousepad", "Save As"])
        ));
        for (other, of_pid) in [
            (
                ("tmp - Thunar".to_owned(), Some(9)),
                titles(&["tmp - Thunar"]),
            ),
            (
                ("Save As".to_owned(), Some(7)),
                titles(&["a.txt - Mousepad", "Save As"]),
            ),
            (
                ("a.txt - Mousepad".to_owned(), Some(8)),
                titles(&["a.txt - Mousepad"]),
            ),
        ] {
            assert!(!is_frame(&mousepad, &other, &of_pid), "{other:?}");
        }
    }

    #[test]
    fn a_process_s_only_x_window_is_its_frame_by_any_title_and_title_twins_are_not() {
        let mousepad = window("a.txt - Mousepad", 7, true);
        let renamed = ("*a.txt - Mousepad".to_owned(), Some(7));
        assert!(is_frame(
            &mousepad,
            &renamed,
            &["*a.txt - Mousepad".to_owned()]
        ));
        let twin = ("a.txt - Mousepad".to_owned(), Some(7));
        let twins = ["a.txt - Mousepad".to_owned(), "a.txt - Mousepad".to_owned()];
        assert!(!is_frame(&mousepad, &twin, &twins));
    }

    #[test]
    fn only_a_process_with_no_frame_is_unlisted() {
        let pids = [Some(814), Some(1649), None, Some(5151)];
        assert!(unlisted(pids.into_iter(), 5173));
        assert!(!unlisted(pids.into_iter(), 5151));
        assert!(!unlisted(pids.into_iter(), 0));
        assert!(unlisted(std::iter::empty(), 5173));
    }

    #[test]
    fn an_x_pid_of_zero_or_of_no_frame_keeps_the_at_spi_state() {
        for pid in [0, 42] {
            let mut frames = vec![
                window("a.txt - Mousepad", 7, true),
                window("Terminal", 9, true),
            ];
            settle_active(&mut frames, Some(pid), "Terminal");
            assert_eq!(actives(&frames), ["a.txt - Mousepad", "Terminal"]);
        }
    }

    struct Seg {
        id: usize,
        size: (u16, u16),
    }

    #[test]
    fn a_resize_or_a_failed_read_opens_a_new_segment_and_a_current_one_is_kept() {
        let screen = std::cell::Cell::new((1280, 800));
        let opened = std::cell::Cell::new(0);
        let fail = std::cell::Cell::new(false);
        let open = || {
            opened.set(opened.get() + 1);
            Ok(Seg {
                id: opened.get(),
                size: screen.get(),
            })
        };
        let current = |g: &Seg| g.size == screen.get();
        let read = |g: &Seg| {
            if fail.replace(false) {
                Err("BadMatch".to_owned())
            } else {
                Ok(g.id)
            }
        };
        let mut slot = None;
        assert_eq!(reuse(&mut slot, open, current, read), Some(1));
        assert_eq!(reuse(&mut slot, open, current, read), Some(1));
        screen.set((1920, 1080));
        assert_eq!(reuse(&mut slot, open, current, read), Some(2));
        assert_eq!(reuse(&mut slot, open, current, read), Some(2));
        fail.set(true);
        assert_eq!(reuse(&mut slot, open, current, read), Some(3));
        assert_eq!(opened.get(), 3);
    }

    struct Logged(usize, std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl Drop for Logged {
        fn drop(&mut self) {
            self.1.borrow_mut().push(format!("drop {}", self.0));
        }
    }

    #[test]
    fn a_lookup_past_its_deadline_is_dropped_and_the_runtime_answers_the_next_call() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let started = std::time::Instant::now();
        let stalled = answered(
            &rt,
            Duration::from_millis(50),
            std::future::pending::<Option<Obj>>(),
        );
        assert_eq!(stalled, None);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            answered(&rt, Duration::from_millis(50), async { Some(7) }),
            Some(Some(7))
        );
    }

    #[test]
    fn a_stale_or_failed_segment_is_released_before_its_replacement_opens() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let opened = std::cell::Cell::new(0);
        let (stale, fail) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
        let open = || {
            opened.set(opened.get() + 1);
            log.borrow_mut().push(format!("open {}", opened.get()));
            Ok(Logged(opened.get(), log.clone()))
        };
        let current = |_: &Logged| !stale.replace(false);
        let read = |g: &Logged| {
            if fail.replace(false) {
                Err("BadAlloc".to_owned())
            } else {
                Ok(g.0)
            }
        };
        let mut slot = None;
        assert_eq!(reuse(&mut slot, open, current, read), Some(1));
        stale.set(true);
        assert_eq!(reuse(&mut slot, open, current, read), Some(2));
        fail.set(true);
        assert_eq!(reuse(&mut slot, open, current, read), Some(3));
        assert_eq!(
            *log.borrow(),
            ["open 1", "drop 1", "open 2", "drop 2", "open 3"]
        );
    }

    #[test]
    fn a_node_keeps_the_atspi_states_role_and_raw_key_binding() {
        let n = atspi::Node {
            obj: Some(Obj::parse("1.17/5").unwrap()),
            role: 43,
            name: "Save".into(),
            states: (1 << 8) | (1 << 25) | (1 << 30),
            ifaces: 0,
            parent: Some(0),
            depth: 2,
            keys: Some("<Alt>s;;<Control>s".into()),
            bounds: Some([1, 2, 3, 4]),
            ..atspi::Node::default()
        };
        let m = node(n);
        assert_eq!(m.id, "1.17/5");
        assert_eq!(m.role, "button");
        assert_eq!(m.states, vec!["showing", "visible"]);
        assert_eq!(m.mnemonic.as_deref(), Some("alt+s"));
        assert_eq!(m.shortcut.as_deref(), Some("ctrl+s"));
        assert_eq!(m.keys.as_deref(), Some("<Alt>s;;<Control>s"));
        assert_eq!(m.bounds, Some([1, 2, 3, 4]));
    }

    #[test]
    fn close_picks_the_one_matching_window_and_refuses_twins() {
        let windows = vec![
            (1, "notes.txt - Mousepad".to_owned()),
            (2, "Save As".to_owned()),
        ];
        assert_eq!(pick_window(&windows, "Save As"), Ok(2));
        assert_eq!(pick_window(&windows, "notes.txt - Mousepad"), Ok(1));
        assert!(pick_window(&windows, "Print").is_err());
        let twins = vec![(1, "Save As".to_owned()), (2, "Save As".to_owned())];
        let err = pick_window(&twins, "Save As").unwrap_err();
        assert!(err.contains("2 windows"), "{err}");
    }

    #[test]
    fn a_frame_takes_the_window_role_its_id_and_active_state() {
        let f = atspi::Frame {
            obj: Obj::parse("1.17/5").unwrap(),
            role: 23,
            name: "Untitled - Mousepad".into(),
            states: 1 << 1,
            pid: Some(42),
        };
        let m = frame(f.clone());
        assert_eq!(
            (m.id.as_str(), m.title.as_str(), m.role, m.pid, m.active),
            ("1.17/5", "Untitled - Mousepad", "window", Some(42), true)
        );
        assert!(!m.unlisted && m.framed);
        let own = std::process::id();
        let mine = frame(atspi::Frame {
            role: 69,
            pid: Some(own),
            ..f
        });
        assert!(!mine.unlisted && !mine.framed);
        assert_eq!(mine.app, app(own));
        assert!(mine.app.is_some());
        assert_eq!(app(u32::MAX), None);
    }
}
