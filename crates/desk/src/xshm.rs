//! Desktop capture over MIT-SHM: the root window of the display read into one shared segment.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{ConnectionExt as _, SK};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xinput::{ConnectionExt as _, DeviceType};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, GrabMode, GrabStatus,
    ImageFormat, ImageOrder, MapState, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::model::Modal;

const OVERLAY_NAME: &[u8] = b"Cua.AgentCursorOverlay";
const BLOCK: usize = 16;
const HASH_MUL: u64 = 0x9e37_79b9_7f4a_7c15;
const RGB_BYTES: u64 = 0x00ff_ffff_00ff_ffff;
const ALL_MASTERS: u16 = 1;

/// A mapped child of the root: a window frame, or a popup such as a menu.
#[derive(Clone, PartialEq)]
pub struct Placement {
    pub id: Window,
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
    fn contains(&self, x: i32, y: i32) -> bool {
        let (left, top) = (i32::from(self.x), i32::from(self.y));
        (left..left + i32::from(self.width)).contains(&x)
            && (top..top + i32::from(self.height)).contains(&y)
    }

    /// Whether the screen rectangle `[x, y, w, h]` lies inside this window.
    pub fn holds(&self, [x, y, w, h]: [i32; 4]) -> bool {
        self.contains(x, y) && self.contains(x + w - 1, y + h - 1)
    }
}

/// One X connection and one shared segment the size of the root window.
pub struct Grabber {
    conn: RustConnection,
    root: Window,
    width: u16,
    height: u16,
    seg: u32,
    addr: NonNull<u8>,
    size: usize,
    net_wm_name: Atom,
    utf8: Atom,
    net_active: Atom,
    net_clients: Atom,
    net_workarea: Atom,
    type_dock: Atom,
    type_desktop: Atom,
    net_wm_pid: Atom,
    net_wm_check: Atom,
    net_stacking: Atom,
    net_wm_state: Atom,
    state_modal: Atom,
    state_hidden: Atom,
    net_wm_type: Atom,
    type_dialog: Atom,
    shell: Mutex<Shell>,
    resized: AtomicBool,
}

// SAFETY: the segment is private to this Grabber and only read through `&self` after a reply.
unsafe impl Send for Grabber {}

impl Grabber {
    /// Connects to `$DISPLAY` and attaches a segment for 32-bit little-endian root pixels.
    pub fn open() -> Result<Self, String> {
        let (conn, n) = x11rb::connect(None).map_err(|e| format!("connect X: {e}"))?;
        let setup = conn.setup();
        let screen = &setup.roots[n];
        let (root, width, height) = (screen.root, screen.width_in_pixels, screen.height_in_pixels);
        let bpp = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == screen.root_depth)
            .map(|f| f.bits_per_pixel);
        if screen.root_depth != 24
            || bpp != Some(32)
            || setup.image_byte_order != ImageOrder::LSB_FIRST
        {
            return Err(format!(
                "unsupported root format: depth {} bpp {bpp:?}",
                screen.root_depth
            ));
        }
        conn.shm_query_version()
            .map_err(|e| format!("MIT-SHM: {e}"))?
            .reply()
            .map_err(|e| format!("MIT-SHM: {e}"))?;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| format!("watch the root window: {e}"))?;
        let seg = conn.generate_id().map_err(|e| format!("X id: {e}"))?;
        let _ = conn.xinput_xi_query_version(2, 2).map(|c| c.reply());
        let [
            net_wm_name,
            utf8,
            net_active,
            net_wm_pid,
            net_wm_check,
            net_stacking,
            net_wm_state,
            state_modal,
            state_hidden,
            net_wm_type,
            type_dialog,
            net_clients,
            net_workarea,
            type_dock,
            type_desktop,
        ] = [
            &b"_NET_WM_NAME"[..],
            b"UTF8_STRING",
            b"_NET_ACTIVE_WINDOW",
            b"_NET_WM_PID",
            b"_NET_SUPPORTING_WM_CHECK",
            b"_NET_CLIENT_LIST_STACKING",
            b"_NET_WM_STATE",
            b"_NET_WM_STATE_MODAL",
            b"_NET_WM_STATE_HIDDEN",
            b"_NET_WM_WINDOW_TYPE",
            b"_NET_WM_WINDOW_TYPE_DIALOG",
            b"_NET_CLIENT_LIST",
            b"_NET_WORKAREA",
            b"_NET_WM_WINDOW_TYPE_DOCK",
            b"_NET_WM_WINDOW_TYPE_DESKTOP",
        ]
        .map(|name| {
            conn.intern_atom(false, name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map_or(u32::from(AtomEnum::NONE), |r| r.atom)
        });
        let size = usize::from(width) * usize::from(height) * 4;
        // SAFETY: a plain syscall that creates a private segment of `size` bytes.
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(format!("shmget: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: `id` is the segment just created; the kernel picks the address.
        let addr = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        let attached = conn
            .shm_attach(seg, u32::try_from(id).unwrap_or_default(), false)
            .map_err(|e| format!("shm attach: {e}"))
            .and_then(|c| c.check().map_err(|e| format!("shm attach: {e}")));
        // SAFETY: marks the segment for removal once both this process and the X server detach.
        unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
        let addr = NonNull::new(addr.cast::<u8>())
            .filter(|a| a.as_ptr() as isize != -1)
            .ok_or_else(|| format!("shmat: {}", std::io::Error::last_os_error()))?;
        let grabber = Self {
            conn,
            root,
            width,
            height,
            seg,
            addr,
            size,
            net_wm_name,
            utf8,
            net_active,
            net_clients,
            net_workarea,
            type_dock,
            type_desktop,
            net_wm_pid,
            net_wm_check,
            net_stacking,
            net_wm_state,
            state_modal,
            state_hidden,
            net_wm_type,
            type_dialog,
            shell: Mutex::new(Shell::default()),
            resized: AtomicBool::new(false),
        };
        attached?;
        grabber.refresh();
        Ok(grabber)
    }

    /// Whether a cua-driver cursor overlay is mapped with a visible shape; the driver's capture leaves it out.
    pub fn overlay_shown(&self) -> Result<bool, String> {
        let tree = self
            .conn
            .query_tree(self.root)
            .map_err(|e| format!("query tree: {e}"))?
            .reply()
            .map_err(|e| format!("query tree: {e}"))?;
        let names = tree
            .children
            .iter()
            .map(|&w| {
                let name = self
                    .conn
                    .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 16)
                    .map_err(|e| format!("get WM_NAME: {e}"))?;
                Ok((w, name))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (w, name) in names {
            let Some(name) = alive(name.reply(), "get WM_NAME")? else {
                continue;
            };
            if !name.value.starts_with(OVERLAY_NAME) {
                continue;
            }
            let attrs = self
                .conn
                .get_window_attributes(w)
                .map_err(|e| format!("window attributes: {e}"))?
                .reply();
            let Some(attrs) = alive(attrs, "window attributes")? else {
                continue;
            };
            if attrs.map_state != MapState::VIEWABLE {
                continue;
            }
            let shape = self
                .conn
                .shape_get_rectangles(w, SK::BOUNDING)
                .map_err(|e| format!("shape: {e}"))?
                .reply();
            let Some(shape) = alive(shape, "shape")? else {
                continue;
            };
            if shape.rectangles.iter().any(|r| r.width > 0 && r.height > 0) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The root window as `(width, height, RGB rows)`.
    pub fn rgb(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let rgb = self.region([0, 0, self.width.into(), self.height.into()])?;
        Ok((u32::from(self.width), u32::from(self.height), rgb))
    }

    /// RGB rows of the screen rectangle `[x, y, w, h]`, which lies inside the screen.
    pub fn region(&self, rect: [i32; 4]) -> Result<Vec<u8>, String> {
        Ok(self
            .image(rect)?
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect())
    }

    /// The root window as one hash per 16-pixel square, row-major; the squares docks cover hash as empty.
    pub fn blocks(&self) -> Result<Vec<u64>, String> {
        let (width, height) = self.size();
        let docks = self.docks();
        Ok(block_hashes(self.pixels()?, width, height, &docks))
    }

    /// The root window as BGRX rows, valid until the next grab.
    pub fn pixels(&self) -> Result<&[u8], String> {
        self.image([0, 0, self.width.into(), self.height.into()])
    }

    /// Whether the root window still has the size the segment was made for, by the root configure events a RandR change sends; no round trip.
    pub fn current(&self) -> bool {
        self.drain();
        !self.resized.load(Ordering::Relaxed)
    }

    /// Which 16-pixel squares a dock window covers, row-major.
    pub fn docks(&self) -> Arc<[bool]> {
        Arc::clone(&self.shell().mask)
    }

    /// The pids that own the desktop shell's windows: the docks and the desktop.
    pub fn shell_pids(&self) -> Vec<u32> {
        self.shell().pids.clone()
    }

    /// The desktop shell's windows, the docks and the desktop, with their window manager frames.
    pub fn shell_windows(&self) -> Vec<Window> {
        self.shell().windows.clone()
    }

    /// The root window's width and height.
    pub fn size(&self) -> (usize, usize) {
        (usize::from(self.width), usize::from(self.height))
    }

    /// The mapped children of the root with their geometry: frames, menus and popups, without the desktop shell.
    pub fn mapped(&self) -> Result<Vec<Placement>, String> {
        self.children(false)
    }

    /// The mapped children of the root with their geometry; the desktop shell's only with `shell`.
    fn children(&self, shell: bool) -> Result<Vec<Placement>, String> {
        let tree = self
            .conn
            .query_tree(self.root)
            .map_err(|e| format!("query tree: {e}"))?
            .reply()
            .map_err(|e| format!("query tree: {e}"))?;
        let cookies = tree
            .children
            .iter()
            .map(|&w| {
                let attrs = self
                    .conn
                    .get_window_attributes(w)
                    .map_err(|e| format!("attributes: {e}"))?;
                let geometry = self
                    .conn
                    .get_geometry(w)
                    .map_err(|e| format!("geometry: {e}"))?;
                Ok((w, attrs, geometry))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let known = (!shell).then(|| self.shell());
        let mut mapped = Vec::new();
        for (id, attrs, geometry) in cookies {
            let (Ok(attrs), Ok(g)) = (attrs.reply(), geometry.reply()) else {
                continue;
            };
            if attrs.map_state == MapState::VIEWABLE
                && known.as_ref().is_none_or(|k| !k.windows.contains(&id))
            {
                mapped.push(Placement {
                    id,
                    x: g.x,
                    y: g.y,
                    width: g.width,
                    height: g.height,
                    popup: attrs.override_redirect,
                    drawn: attrs.class == WindowClass::INPUT_OUTPUT,
                });
            }
        }
        Ok(mapped)
    }

    /// The title of window `w`, or of the client inside it when `w` is a window manager frame.
    pub fn title(&self, w: Window) -> String {
        let own = self.name(w);
        if !own.is_empty() {
            return own;
        }
        let children = self.kids(w);
        children
            .iter()
            .map(|&c| self.name(c))
            .find(|n| !n.is_empty())
            .unwrap_or_default()
    }

    /// The window `_NET_ACTIVE_WINDOW` names, the one with keyboard focus.
    pub fn active(&self) -> Option<Window> {
        let reply = self
            .conn
            .get_property(false, self.root, self.net_active, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        reply.value32()?.next().filter(|w| *w != 0)
    }

    /// Whether a window manager runs: the root's `_NET_SUPPORTING_WM_CHECK` names a window that exists.
    pub fn wm(&self) -> Result<bool, String> {
        let check = self
            .conn
            .get_property(false, self.root, self.net_wm_check, AtomEnum::WINDOW, 0, 1)
            .map_err(|e| format!("get _NET_SUPPORTING_WM_CHECK: {e}"))?
            .reply()
            .map_err(|e| format!("get _NET_SUPPORTING_WM_CHECK: {e}"))?;
        let Some(w) = check
            .value32()
            .and_then(|mut v| v.next())
            .filter(|w| *w != 0)
        else {
            return Ok(false);
        };
        let attrs = self
            .conn
            .get_window_attributes(w)
            .map_err(|e| format!("window attributes: {e}"))?
            .reply();
        Ok(alive(attrs, "window attributes")?.is_some())
    }

    pub fn dialog(&self, w: Window) -> bool {
        let (kind, dialog) = (self.net_wm_type, self.type_dialog);
        let children = self.kids(w);
        std::iter::once(w).chain(children).any(|c| {
            let get = |property: u32, kind: u32| {
                self.conn
                    .get_property(false, c, property, kind, 0, 16)
                    .ok()
                    .and_then(|r| r.reply().ok())
            };
            let transient = get(AtomEnum::WM_TRANSIENT_FOR.into(), AtomEnum::WINDOW.into())
                .and_then(|r| r.value32().and_then(|mut v| v.next()))
                .is_some_and(|v| v != 0);
            let typed = get(kind, AtomEnum::ATOM.into())
                .and_then(|r| r.value32().map(|v| v.collect::<Vec<_>>()))
                .is_some_and(|v| v.contains(&dialog));
            transient || typed
        })
    }

    /// The modal dialogs the screen does not show: hidden, centred off the screen, or under another
    /// window at their centre.
    pub fn modals(&self) -> Result<Vec<Modal>, String> {
        let clients = self.clients()?;
        let states = clients
            .iter()
            .map(|&c| {
                self.conn
                    .get_property(false, c, self.net_wm_state, AtomEnum::ATOM, 0, 32)
                    .map_err(|e| format!("get _NET_WM_STATE: {e}"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut out = Vec::new();
        for (&c, state) in clients.iter().zip(states) {
            let Some(state) = alive(state.reply(), "get _NET_WM_STATE")? else {
                continue;
            };
            let atoms: Vec<Atom> = state.value32().map(Iterator::collect).unwrap_or_default();
            if !atoms.contains(&self.state_modal) {
                continue;
            }
            let place = if atoms.contains(&self.state_hidden) {
                "hidden".to_owned()
            } else {
                match self.unseen(c)? {
                    Some(place) => place,
                    None => continue,
                }
            };
            let owner = self
                .conn
                .get_property(false, c, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)
                .ok()
                .and_then(|r| r.reply().ok())
                .and_then(|r| r.value32().and_then(|mut v| v.next()))
                .filter(|w| *w != 0)
                .map(|w| self.title(w))
                .unwrap_or_default();
            out.push(Modal {
                title: self.title(c),
                pid: self.pid(c),
                owner,
                place,
            });
        }
        Ok(out)
    }

    /// The windows the window manager manages, bottom to top.
    fn clients(&self) -> Result<Vec<Window>, String> {
        Ok(self
            .conn
            .get_property(
                false,
                self.root,
                self.net_stacking,
                AtomEnum::WINDOW,
                0,
                1024,
            )
            .map_err(|e| format!("get _NET_CLIENT_LIST_STACKING: {e}"))?
            .reply()
            .map_err(|e| format!("get _NET_CLIENT_LIST_STACKING: {e}"))?
            .value32()
            .map(Iterator::collect)
            .unwrap_or_default())
    }

    /// Where the screen does not show the client `c`; `None` when it shows or is not mapped.
    fn unseen(&self, c: Window) -> Result<Option<String>, String> {
        let frame = self.top_level(c)?;
        let screen = (i32::from(self.width), i32::from(self.height));
        Ok(match cover(frame, &self.children(false)?, screen) {
            Some(Cover::OffScreen) => Some("off the screen".to_owned()),
            Some(Cover::Under(w)) => Some(format!("behind \"{}\"", self.title(w))),
            None => None,
        })
    }

    /// The WM_CLASS class of `w`, such as `Google-chrome`, or of the client inside it when `w` is a window manager frame.
    pub fn class(&self, w: Window) -> String {
        let children = self.kids(w);
        std::iter::once(w)
            .chain(children)
            .find_map(|c| {
                let value = self
                    .conn
                    .get_property(false, c, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
                    .ok()?
                    .reply()
                    .ok()?
                    .value;
                let class = value.split(|b| *b == 0).nth(1)?;
                (!class.is_empty()).then(|| String::from_utf8_lossy(class).into_owned())
            })
            .unwrap_or_default()
    }

    /// Whether keys sent to the focused window reach `window` of `pid`: it is the active window and holds
    /// the input focus, no popup of another client is on the screen, the keyboard is not grabbed unless
    /// by a popup of `pid`, and the core keyboard is the only master, since Chrome drops core keyboard
    /// input while another one exists.
    pub fn keyboard(&self, window: Window, pid: u32) -> Result<bool, String> {
        if self.active() != Some(window) || self.masters() != Some(1) {
            return Ok(false);
        }
        let focus = self
            .conn
            .get_input_focus()
            .map_err(|e| format!("input focus: {e}"))?
            .reply()
            .map_err(|e| format!("input focus: {e}"))?
            .focus;
        if !self.inside(focus, window)? {
            return Ok(false);
        }
        let (width, height) = (i32::from(self.width), i32::from(self.height));
        let popups: Vec<Option<u32>> = self
            .children(true)?
            .iter()
            .filter(|p| p.popup && p.drawn)
            .filter(|p| {
                let (x, y) = (i32::from(p.x), i32::from(p.y));
                x < width && y < height && x + i32::from(p.width) > 0 && y + i32::from(p.height) > 0
            })
            .map(|p| self.pid(p.id))
            .collect();
        if popups.iter().any(|p| *p != Some(pid)) {
            return Ok(false);
        }
        Ok(!popups.is_empty() || !self.grabbed(false)?)
    }

    /// Whether another client holds an active grab of the keyboard, or of the pointer with `pointer`;
    /// a probe grab of this connection that succeeds is released at once.
    pub fn grabbed(&self, pointer: bool) -> Result<bool, String> {
        let (async_, now) = (GrabMode::ASYNC, x11rb::CURRENT_TIME);
        let status = if pointer {
            let mask = EventMask::NO_EVENT;
            self.conn
                .grab_pointer(false, self.root, mask, async_, async_, 0u32, 0u32, now)
                .map_err(|e| format!("grab pointer: {e}"))?
                .reply()
                .map_err(|e| format!("grab pointer: {e}"))?
                .status
        } else {
            self.conn
                .grab_keyboard(false, self.root, now, async_, async_)
                .map_err(|e| format!("grab keyboard: {e}"))?
                .reply()
                .map_err(|e| format!("grab keyboard: {e}"))?
                .status
        };
        if status == GrabStatus::SUCCESS {
            let released = if pointer {
                self.conn.ungrab_pointer(now)
            } else {
                self.conn.ungrab_keyboard(now)
            };
            released.map_err(|e| format!("ungrab: {e}"))?;
            self.conn.flush().map_err(|e| format!("ungrab: {e}"))?;
        }
        Ok(status != GrabStatus::SUCCESS)
    }

    /// The number of master keyboards.
    fn masters(&self) -> Option<usize> {
        let reply = self
            .conn
            .xinput_xi_query_device(ALL_MASTERS)
            .ok()?
            .reply()
            .ok()?;
        Some(
            reply
                .infos
                .iter()
                .filter(|d| d.type_ == DeviceType::MASTER_KEYBOARD)
                .count(),
        )
    }

    /// The top drawn child of the root at the screen point `(x, y)`, past a cua-driver cursor overlay, which input passes through.
    pub fn top_at(&self, x: i32, y: i32) -> Result<Option<Placement>, String> {
        Ok(self.children(true)?.into_iter().rev().find(|p| {
            p.drawn && p.contains(x, y) && !self.name(p.id).as_bytes().starts_with(OVERLAY_NAME)
        }))
    }

    /// Whether the screen point `(x, y)` shows a window title rather than a window's content: a desktop
    /// panel, whose task buttons carry the titles, or the band of a window manager frame above its client.
    pub fn title_at(&self, x: i32, y: i32) -> Result<bool, String> {
        let Some(top) = self.top_at(x, y)? else {
            return Ok(false);
        };
        if top.popup {
            return Ok(false);
        }
        if self.shell().docks.contains(&top.id) {
            return Ok(true);
        }
        Ok(y < i32::from(top.y) + i32::from(self.client_offset(top.id)))
    }

    /// How far below the top of frame `w` its client starts, the height of its title bar; 0 for a window
    /// that is its own client.
    fn client_offset(&self, w: Window) -> i16 {
        let children = self.kids(w);
        children
            .into_iter()
            .find(|&c| {
                self.conn
                    .get_property(false, c, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1)
                    .ok()
                    .and_then(|r| r.reply().ok())
                    .is_some_and(|r| !r.value.is_empty())
            })
            .and_then(|c| self.conn.get_geometry(c).ok()?.reply().ok())
            .map_or(0, |g| g.y)
    }

    /// The child of the root that holds window `w`: its window manager frame, or `w` itself.
    pub fn top_level(&self, mut w: Window) -> Result<Window, String> {
        loop {
            let parent = self.parent(w)?;
            if parent == self.root || parent == x11rb::NONE {
                return Ok(w);
            }
            w = parent;
        }
    }

    /// The `_NET_WM_PID` of window `w`.
    pub fn pid(&self, w: Window) -> Option<u32> {
        let reply = self
            .conn
            .get_property(false, w, self.net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        reply.value32()?.next()
    }

    /// Whether `w` is `ancestor` or lies inside it; false for no window and for the pointer root.
    fn inside(&self, mut w: Window, ancestor: Window) -> Result<bool, String> {
        loop {
            if w == ancestor {
                return Ok(true);
            }
            if w == self.root || w <= 1 {
                return Ok(false);
            }
            w = self.parent(w)?;
        }
    }

    /// The parent of `w`; none once `w` is gone.
    fn parent(&self, w: Window) -> Result<Window, String> {
        let tree = self
            .conn
            .query_tree(w)
            .map_err(|e| format!("query tree: {e}"))?
            .reply();
        Ok(alive(tree, "query tree")?.map_or(x11rb::NONE, |t| t.parent))
    }

    /// The desktop shell as the last refresh read it, after the pending events.
    fn shell(&self) -> std::sync::MutexGuard<'_, Shell> {
        self.drain();
        self.shell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drains the pending events without a round trip; a dock or client list change reads the shell again.
    fn drain(&self) {
        let mut changed = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            if resized(&event, self.root, (self.width, self.height)) {
                self.resized.store(true, Ordering::Relaxed);
            }
            changed |= match &event {
                Event::PropertyNotify(p) => [self.net_clients, self.net_workarea].contains(&p.atom),
                Event::ConfigureNotify(c) => c.window != self.root,
                Event::MapNotify(_) | Event::UnmapNotify(_) | Event::DestroyNotify(_) => true,
                _ => false,
            };
        }
        if changed {
            self.refresh();
        }
    }

    /// Reads the shell's windows by type, their pids and frames, and the dock mask, and watches each dock for moves.
    fn refresh(&self) {
        let clients = self.clients().unwrap_or_default();
        let types: Vec<_> = clients
            .iter()
            .map(|&c| {
                self.conn
                    .get_property(false, c, self.net_wm_type, AtomEnum::ATOM, 0, 16)
                    .ok()
            })
            .collect();
        let (mut next, mut rects) = (Shell::default(), Vec::new());
        for (&c, kind) in clients.iter().zip(types) {
            let kinds: Vec<Atom> = kind
                .and_then(|k| k.reply().ok())
                .and_then(|r| r.value32().map(Iterator::collect))
                .unwrap_or_default();
            let dock = kinds.contains(&self.type_dock);
            if !dock && !kinds.contains(&self.type_desktop) {
                continue;
            }
            if let Some(pid) = self.pid(c).filter(|p| !next.pids.contains(p)) {
                next.pids.push(pid);
            }
            let frame = self
                .parent(c)
                .ok()
                .filter(|&f| f != self.root && f != x11rb::NONE);
            let ids: Vec<Window> = std::iter::once(c).chain(frame).collect();
            if dock {
                for &w in &ids {
                    let watch =
                        ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
                    let _ = self.conn.change_window_attributes(w, &watch);
                }
                rects.extend(self.on_root(c));
                next.docks.extend(&ids);
            }
            next.windows.extend(ids);
        }
        let _ = self.conn.flush();
        next.mask = Arc::from(dock_mask(
            &rects,
            (usize::from(self.width), usize::from(self.height)),
        ));
        *self
            .shell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    /// The screen rectangle `[x, y, w, h]` of window `w` while it is mapped.
    fn on_root(&self, w: Window) -> Option<[i32; 4]> {
        let attrs = self.conn.get_window_attributes(w).ok()?;
        let geometry = self.conn.get_geometry(w).ok()?;
        let at = self.conn.translate_coordinates(w, self.root, 0, 0).ok()?;
        if attrs.reply().ok()?.map_state != MapState::VIEWABLE {
            return None;
        }
        let (g, at) = (geometry.reply().ok()?, at.reply().ok()?);
        Some([
            at.dst_x.into(),
            at.dst_y.into(),
            g.width.into(),
            g.height.into(),
        ])
    }

    fn kids(&self, w: Window) -> Vec<Window> {
        self.conn
            .query_tree(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|t| t.children)
            .unwrap_or_default()
    }

    fn name(&self, w: Window) -> String {
        for (atom, kind) in [
            (self.net_wm_name, self.utf8),
            (u32::from(AtomEnum::WM_NAME), u32::from(AtomEnum::STRING)),
        ] {
            let value = self
                .conn
                .get_property(false, w, atom, kind, 0, 64)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.value)
                .unwrap_or_default();
            if !value.is_empty() {
                return String::from_utf8_lossy(&value).into_owned();
            }
        }
        String::new()
    }

    fn image(&self, [x, y, w, h]: [i32; 4]) -> Result<&[u8], String> {
        let narrow = |v: i32| i16::try_from(v).map_err(|_| format!("rectangle {x},{y} {w}x{h}"));
        let wide = |v: i32| u16::try_from(v).map_err(|_| format!("rectangle {x},{y} {w}x{h}"));
        let (rx, ry, rw, rh) = (narrow(x)?, narrow(y)?, wide(w)?, wide(h)?);
        self.conn
            .shm_get_image(
                self.root,
                rx,
                ry,
                rw,
                rh,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                self.seg,
                0,
            )
            .map_err(|e| format!("shm get image: {e}"))?
            .reply()
            .map_err(|e| format!("shm get image: {e}"))?;
        let len = usize::from(rw) * usize::from(rh) * 4;
        if len > self.size {
            return Err(format!("rectangle {x},{y} {w}x{h} exceeds the screen"));
        }
        // SAFETY: the reply means the server finished writing `len` bytes; the segment spans `size` bytes for the life of `self`.
        Ok(unsafe { std::slice::from_raw_parts(self.addr.as_ptr(), len) })
    }
}

impl Drop for Grabber {
    fn drop(&mut self) {
        let _ = self.conn.shm_detach(self.seg);
        let _ = self.conn.flush();
        // SAFETY: `addr` came from shmat and is not used after this.
        unsafe { libc::shmdt(self.addr.as_ptr().cast()) };
    }
}

/// The desktop shell as one refresh read it.
#[derive(Default)]
struct Shell {
    /// Which 16-pixel squares a dock covers, row-major.
    mask: Arc<[bool]>,
    pids: Vec<u32>,
    /// The dock windows and their window manager frames.
    docks: Vec<Window>,
    /// The docks and the desktop windows, with their frames.
    windows: Vec<Window>,
}

/// How the screen hides a mapped window frame.
#[derive(Debug, PartialEq)]
enum Cover {
    OffScreen,
    Under(Window),
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

/// Which 16-pixel squares of a `width` by `height` screen, row-major, a dock rectangle of `docks` (`[x, y, w, h]`) overlaps.
fn dock_mask(docks: &[[i32; 4]], (width, height): (usize, usize)) -> Vec<bool> {
    let columns = width.div_ceil(BLOCK);
    let mut mask = vec![false; columns * height.div_ceil(BLOCK)];
    let clip = |v: i32, max: usize| usize::try_from(v.max(0)).unwrap_or(0).min(max);
    for &[x, y, w, h] in docks {
        let (x0, x1) = (clip(x, width), clip(x.saturating_add(w), width));
        let (y0, y1) = (clip(y, height), clip(y.saturating_add(h), height));
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        for row in y0 / BLOCK..y1.div_ceil(BLOCK) {
            mask[row * columns + x0 / BLOCK..row * columns + x1.div_ceil(BLOCK)].fill(true);
        }
    }
    mask
}

/// How `mapped`, the drawn root children bottom to top, hide `frame` on a screen of `width` by
/// `height`: centred off the screen, or under another managed window at its centre.
fn cover(frame: Window, mapped: &[Placement], (width, height): (i32, i32)) -> Option<Cover> {
    let own = mapped.iter().find(|p| p.id == frame)?;
    let (cx, cy) = (
        i32::from(own.x) + i32::from(own.width) / 2,
        i32::from(own.y) + i32::from(own.height) / 2,
    );
    if !(0..width).contains(&cx) || !(0..height).contains(&cy) {
        return Some(Cover::OffScreen);
    }
    mapped
        .iter()
        .rev()
        .find(|p| p.drawn && !p.popup && p.contains(cx, cy))
        .filter(|p| p.id != frame)
        .map(|p| Cover::Under(p.id))
}

fn mix(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(HASH_MUL).rotate_left(29)
}

/// The reply to a request about a window; `None` when the window closed after it was listed (BadWindow).
fn alive<T>(reply: Result<T, ReplyError>, what: &str) -> Result<Option<T>, String> {
    match reply {
        Ok(v) => Ok(Some(v)),
        Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Window => Ok(None),
        Err(e) => Err(format!("{what}: {e}")),
    }
}

/// Whether `event` reconfigures the root window `root` to a size other than `size`.
fn resized(event: &Event, root: Window, size: (u16, u16)) -> bool {
    matches!(event, Event::ConfigureNotify(c) if c.window == root && (c.width, c.height) != size)
}

#[cfg(test)]
mod tests {
    use x11rb::x11_utils::X11Error;

    use super::*;

    #[test]
    fn a_root_configure_to_another_size_is_a_resize_and_a_dock_configure_is_not() {
        let configure = |window, width, height| {
            Event::ConfigureNotify(x11rb::protocol::xproto::ConfigureNotifyEvent {
                window,
                width,
                height,
                ..Default::default()
            })
        };
        assert!(resized(&configure(1, 1920, 1080), 1, (1280, 800)));
        assert!(!resized(&configure(1, 1280, 800), 1, (1280, 800)));
        assert!(!resized(&configure(7, 306, 49), 1, (1280, 800)));
        assert!(!resized(
            &Event::MapNotify(Default::default()),
            1,
            (1280, 800)
        ));
    }

    fn x11_error(error_kind: ErrorKind) -> ReplyError {
        ReplyError::X11Error(X11Error {
            error_kind,
            error_code: 3,
            sequence: 37,
            bad_value: 25_165_899,
            minor_opcode: 0,
            major_opcode: 20,
            extension_name: None,
            request_name: Some("GetProperty"),
        })
    }

    #[test]
    fn one_changed_pixel_changes_only_its_block_and_padding_and_masked_blocks_do_not_count() {
        let (w, h) = (40, 120);
        let frame = vec![200u8; w * h * 4];
        let skip = dock_mask(&[[0, 0, 40, 27], [16, 100, 10, 20]], (w, h));
        let base = block_hashes(&frame, w, h, &skip);
        assert_eq!(base.len(), 3 * 8);
        let mut one = frame.clone();
        one[(50 * w + 33) * 4 + 1] = 199;
        let changed: Vec<usize> = (0..base.len())
            .filter(|&i| block_hashes(&one, w, h, &skip)[i] != base[i])
            .collect();
        assert_eq!(changed, [3 * 3 + 2]);
        let mut padding = frame.clone();
        padding[(50 * w + 3) * 4 + 3] = 0;
        padding[(10 * w + 3) * 4] = 0;
        padding[(110 * w + 20) * 4] = 0;
        assert_eq!(block_hashes(&padding, w, h, &skip), base);
        let beside = {
            let mut f = frame.clone();
            f[(110 * w + 3) * 4] = 0;
            f
        };
        assert_ne!(block_hashes(&beside, w, h, &skip), base);
        assert_ne!(
            block_hashes(&padding, w, h, &[]),
            block_hashes(&frame, w, h, &[])
        );
    }

    #[test]
    fn the_mask_covers_the_blocks_a_dock_overlaps_and_nothing_beside_a_centred_dock() {
        let (top, dock, hidden) = ([0, 0, 1280, 27], [487, 751, 306, 49], [-9999, -9999, 3, 3]);
        let mask = dock_mask(&[top, dock, hidden], (1280, 800));
        let columns = 80;
        assert_eq!(mask.len(), columns * 50);
        let masked = |row: usize| -> Vec<usize> {
            (0..columns).filter(|&c| mask[row * columns + c]).collect()
        };
        assert_eq!(masked(0).len(), columns);
        assert_eq!(masked(1).len(), columns);
        assert!(masked(2).is_empty());
        assert!(masked(45).is_empty());
        assert_eq!(masked(46), (30..50).collect::<Vec<_>>());
        assert_eq!(masked(49), (30..50).collect::<Vec<_>>());
        assert_eq!(mask.iter().filter(|m| **m).count(), 2 * columns + 4 * 20);
        assert!(dock_mask(&[], (1280, 800)).iter().all(|m| !m));
    }

    #[test]
    fn a_rectangle_clips_to_the_screen_in_whole_pixels() {
        assert_eq!(
            clip([10.5, 20.2, 30.1, 40.0], (1280, 800)),
            Some([10, 20, 31, 41])
        );
        assert_eq!(
            clip([0.0, 0.0, f64::MAX / 4.0, f64::MAX / 4.0], (1280, 800)),
            Some([0, 0, 1280, 800])
        );
        assert_eq!(clip([-1.5, 0.5, 3.0, 9.0], (4, 2)), Some([0, 0, 2, 2]));
        assert_eq!(clip([1300.0, 0.0, 10.0, 10.0], (1280, 800)), None);
    }

    #[test]
    fn a_block_differs_visibly_only_past_the_step_in_a_colour_channel() {
        let (w, h) = (40, 120);
        let frame = vec![200u8; w * h * 4];
        let mut faint = frame.clone();
        faint[(50 * w + 33) * 4 + 1] = 180;
        faint[(50 * w + 3) * 4 + 3] = 0;
        assert!(!visible(&frame, &faint, w, h, 3 * 3 + 2, 24));
        assert!(!visible(&frame, &faint, w, h, 3 * 3, 24));
        let mut seen = faint.clone();
        seen[(50 * w + 33) * 4 + 2] = 150;
        assert!(visible(&frame, &seen, w, h, 3 * 3 + 2, 24));
        assert!(!visible(&frame, &seen, w, h, 3 * 3 + 1, 24));
        assert!(visible(&frame, &seen[4..], w, h, 0, 24));
    }

    #[test]
    fn a_frame_is_hidden_when_centred_off_the_screen_or_under_another_window() {
        let at = |id: Window, x: i16, y: i16, popup: bool| Placement {
            id,
            x,
            y,
            width: 400,
            height: 300,
            popup,
            drawn: true,
        };
        let screen = (1280, 800);
        let dialog = at(9, 100, 100, false);
        assert_eq!(
            cover(9, &[at(5, 0, 0, false), dialog.clone()], screen),
            None
        );
        assert_eq!(
            cover(9, &[dialog.clone(), at(5, 150, 150, false)], screen),
            Some(Cover::Under(5))
        );
        assert_eq!(
            cover(9, &[dialog.clone(), at(5, 150, 150, true)], screen),
            None
        );
        assert_eq!(
            cover(9, &[dialog.clone(), at(5, 600, 150, false)], screen),
            None
        );
        assert_eq!(
            cover(9, &[at(9, 1100, 100, false)], screen),
            Some(Cover::OffScreen)
        );
        assert_eq!(
            cover(9, &[at(9, -250, 100, false)], screen),
            Some(Cover::OffScreen)
        );
        assert_eq!(cover(9, &[at(5, 0, 0, false)], screen), None);
    }

    #[test]
    fn a_window_gone_before_its_reply_is_skipped_and_other_errors_are_not() {
        assert_eq!(alive(Ok(7), "get WM_NAME"), Ok(Some(7)));
        assert_eq!(
            alive::<u8>(Err(x11_error(ErrorKind::Window)), "get WM_NAME"),
            Ok(None)
        );
        let other = alive::<u8>(Err(x11_error(ErrorKind::Match)), "get WM_NAME");
        assert!(
            matches!(&other, Err(e) if e.starts_with("get WM_NAME: ")),
            "{other:?}"
        );
    }
}
