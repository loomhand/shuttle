//! The macOS desk backend: window trees by a budgeted AX walk with one batched attribute read per element.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_foundation::array::{CFArray, CFArrayCreate, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFHash, CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::CFURL;

use crate::ax_roles::{self, Facts, Kind};
use crate::keys::ax_combo;
use crate::model::{
    Access, Act, Extras, Frame, MAX_DEPTH, MAX_IDS, MAX_NODES, Node, Tree, VALUE_CHARS,
    format_number, is_active, lookup, place,
};

const WALK_BUDGET: Duration = Duration::from_secs(4);
const MESSAGING_TIMEOUT: Duration = Duration::from_millis(500);
const HUNG_SKIP: Duration = Duration::from_secs(10);
const HUNG_AFTER: Duration = Duration::from_secs(2);
const LAUNCH_GRACE: Duration = Duration::from_secs(10);
const AX_SUCCESS: i32 = 0;
const AX_CANNOT_COMPLETE: i32 = -25204;
const AX_NO_VALUE: i32 = -25212;

const POINT_TYPE: u32 = 1;
const SIZE_TYPE: u32 = 2;
const ON_SCREEN_ONLY: u32 = 1;
const EXCLUDE_DESKTOP: u32 = 1 << 4;
const OVERLAY_LEVEL: i64 = 102;
const PATH_MAX: usize = 4096;
const PATH_MAX_DEPTH: usize = 16;
const NAME_CHARS: usize = 200;
const WEB_SWITCHES: [&str; 2] = ["AXManualAccessibility", "AXEnhancedUserInterface"];
const ATTRS: [&str; 17] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXHelp",
    "AXValue",
    "AXEnabled",
    "AXFocused",
    "AXSelected",
    "AXExpanded",
    "AXPosition",
    "AXSize",
    "AXChildren",
    "AXMenuItemCmdChar",
    "AXMenuItemCmdModifiers",
    "AXMenuItemMarkChar",
    "AXModal",
];
const APP_ATTRS: [&str; 5] = [
    "AXTitle",
    "AXFrontmost",
    "AXFocusedWindow",
    "AXMainWindow",
    "AXWindows",
];
const FRAME_ATTRS: [&str; 6] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXChildren",
    "AXPosition",
    "AXSize",
];

static PROMPTED: AtomicBool = AtomicBool::new(false);
static CGS: OnceLock<Option<Cgs>> = OnceLock::new();

/// The AX backend: element ids are `pid/n`, kept while this backend lives.
pub struct Ax {
    names: CFArray<CFString>,
    app_names: CFArray<CFString>,
    frame_names: CFArray<CFString>,
    ids: HashMap<String, CFType>,
    hashes: HashMap<usize, Vec<(CFType, String)>>,
    next: u64,
    hung: HashMap<i32, Instant>,
    slow: HashMap<i32, Instant>,
    last: HashMap<i32, Vec<Frame>>,
    listed_at: HashMap<i32, Instant>,
    driver: Option<PathBuf>,
}

impl Ax {
    /// The backend, once this process holds the Accessibility permission.
    pub fn open() -> Result<Self, String> {
        if !trusted(!PROMPTED.swap(true, Ordering::Relaxed)) {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Err(format!(
                "no Accessibility permission for {exe}: allow it in System Settings > Privacy & Security > Accessibility"
            ));
        }
        set_timeout(&system_wide());
        Ok(Self {
            names: names(ATTRS),
            app_names: names(APP_ATTRS),
            frame_names: names(FRAME_ATTRS),
            ids: HashMap::new(),
            hashes: HashMap::new(),
            next: 0,
            hung: HashMap::new(),
            slow: HashMap::new(),
            last: HashMap::new(),
            listed_at: HashMap::new(),
            driver: std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .ok()
                .map(|e| e.with_file_name("cua-driver")),
        })
    }

    fn listed(&mut self) -> Vec<(i32, Vec<[f64; 4]>)> {
        let shown = shown_windows();
        let has = |p: &i32| shown.iter().any(|(q, _)| q == p);
        self.hung.retain(|_, at| at.elapsed() < HUNG_SKIP);
        self.last.retain(|p, _| has(p));
        self.slow.retain(|p, _| has(p));
        self.listed_at.retain(|p, _| has(p));
        for (p, _) in &shown {
            self.listed_at.entry(*p).or_insert_with(Instant::now);
        }
        let driver = self.driver.as_ref();
        shown
            .into_iter()
            .filter(|(p, _)| driver.is_none_or(|d| exe_path(*p).as_ref() != Some(d)))
            .collect()
    }

    fn app_frames(&mut self, apps: impl IntoIterator<Item = (i32, Vec<[f64; 4]>)>) -> Vec<Frame> {
        let mut frames = Vec::new();
        let scale = display_scale();
        for (p, bounds) in apps {
            if self.hung.contains_key(&p) {
                continue;
            }
            let app = application(p);
            let asked = Instant::now();
            let read = values(&self.app_names, &app);
            if read.iter().all(Option::is_none) && asked.elapsed() >= MESSAGING_TIMEOUT * 9 / 10 {
                let slow = self.slow.entry(p).or_insert(asked).elapsed();
                let listed = self
                    .listed_at
                    .get(&p)
                    .map_or(LAUNCH_GRACE, Instant::elapsed);
                if hung(slow, listed) {
                    self.slow.remove(&p);
                    self.last.remove(&p);
                    self.hung.insert(p, Instant::now());
                } else {
                    let kept = self.last.get(&p).into_iter().flatten();
                    frames.extend(kept.map(|f| Frame {
                        stale: true,
                        ..f.clone()
                    }));
                }
                continue;
            }
            self.slow.remove(&p);
            let from = frames.len();
            let [name, front, focused, main, windows] = read;
            let name = name
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string());
            let front = boolean(front).unwrap_or(false);
            let focused = focused.or(main);
            let Some(windows) = windows.map(|w| items(&w)) else {
                continue;
            };
            for w in windows.iter() {
                let [role, subrole, title, children, pos, size] = values(&self.frame_names, w);
                let sheets = sheets(children.map(|c| items(&c)).unwrap_or_default());
                let mine = focused
                    .as_ref()
                    .is_some_and(|f| f == w || sheets.iter().any(|(s, _)| s == f));
                let mut active = front && mine;
                let title = string(title).trim().to_owned();
                let owner = self.id_of(w);
                let ids: Vec<String> = sheets.iter().map(|(s, _)| self.id_of(s)).collect();
                for (k, (sheet, parent)) in sheets.iter().enumerate().rev() {
                    let s = attrs(&self.names, sheet);
                    frames.push(Frame {
                        id: ids[k].clone(),
                        title: [s.title, s.description]
                            .into_iter()
                            .find(|t| !t.is_empty())
                            .unwrap_or_else(|| title.clone()),
                        role: "dialog",
                        pid: Some(p as u32),
                        active,
                        app: name.clone(),
                        owner: Some(parent.map_or_else(|| owner.clone(), |at| ids[at].clone())),
                        ..Frame::default()
                    });
                    active = false;
                }
                let (role, subrole) = (string(role), string(subrole));
                let kind = Kind {
                    role: &role,
                    subrole: &subrole,
                    ..Kind::default()
                };
                let frame = pos
                    .and_then(|v| pair(&v, POINT_TYPE))
                    .zip(size.and_then(|v| pair(&v, SIZE_TYPE)))
                    .map(|((x, y), (w, h))| [x, y, w, h]);
                frames.push(Frame {
                    id: owner,
                    title,
                    role: ax_roles::role(kind).unwrap_or("window"),
                    pid: Some(p as u32),
                    active,
                    app: name.clone(),
                    hidden: !shows(&bounds, frame),
                    number: number_of(w),
                    bounds: frame.map(|f| f.map(|v| (v * scale).round() as i32)),
                    ..Frame::default()
                });
            }
            self.last.insert(p, frames[from..].to_vec());
        }
        frames
    }

    fn id_of(&mut self, el: &CFType) -> String {
        let hash = hash_of(el);
        if let Some((_, id)) = self
            .hashes
            .get(&hash)
            .and_then(|v| v.iter().find(|(e, _)| e == el))
        {
            return id.clone();
        }
        if self.ids.len() >= MAX_IDS {
            self.ids.clear();
            self.hashes.clear();
        }
        self.next += 1;
        let id = format!("{}/{}", pid(el).unwrap_or(0), self.next);
        self.ids.insert(id.clone(), el.clone());
        self.hashes
            .entry(hash)
            .or_default()
            .push((el.clone(), id.clone()));
        id
    }

    fn element(&self, id: &str) -> Result<CFType, String> {
        lookup(&self.ids, id)
    }

    fn node_of(
        &mut self,
        el: &CFType,
        a: &Attrs,
        extras: Extras,
        chars: usize,
        scale: f64,
    ) -> Node {
        let kind = kind_of(a);
        let mapped = ax_roles::role(kind);
        let role = mapped.unwrap_or("generic");
        let shown = a.role != "AXMenuItem" && a.size.is_some_and(|(w, h)| w > 0.0 && h > 0.0);
        let number = a
            .value
            .as_ref()
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64());
        let facts = Facts {
            enabled: a.enabled.unwrap_or(true),
            focused: a.focused,
            selected: a.selected,
            expanded: a.expanded,
            modal: a.modal,
            editable: matches!(a.role.as_str(), "AXTextField" | "AXTextArea"),
            shown,
            value: if kind.marked { Some(1) } else { number },
            secure: [&a.role, &a.subrole]
                .iter()
                .any(|r| *r == "AXSecureTextField"),
        };
        let name = name_of(a, chars);
        let bounds = match (a.pos, a.size) {
            (Some((x, y)), Some((w, h)))
                if extras.bounds && shown && (extras.frame || is_active(role)) =>
            {
                Some([x, y, w, h].map(|v| (v * scale).round() as i32))
            }
            _ => None,
        };
        Node {
            id: self.id_of(el),
            role,
            native_role: mapped.is_none().then(|| a.role.clone()),
            name,
            description: a.help.clone(),
            states: ax_roles::states(role, facts),
            value: extras
                .values
                .then(|| a.value.as_ref().map(|v| text(v, chars)))
                .flatten()
                .filter(|v| !v.is_empty()),
            shortcut: extras
                .keys
                .then(|| ax_combo(&a.cmd_char, a.cmd_modifiers.unwrap_or(0)))
                .flatten(),
            bounds,
            ..Node::default()
        }
    }

    fn walk(&mut self, w: &mut Walk, el: &CFType, parent: Option<usize>, depth: usize) -> bool {
        if w.nodes.len() >= MAX_NODES || Instant::now() >= w.deadline {
            return true;
        }
        if depth > MAX_DEPTH || w.path.contains(el) {
            return false;
        }
        let a = attrs(&self.names, el);
        let node = self.node_of(el, &a, w.extras, VALUE_CHARS, w.scale);
        let (parent, depth) = place(&mut w.nodes, node, parent, depth);
        w.path.push(el.clone());
        let mut truncated = false;
        for child in &a.children {
            truncated |= self.walk(w, child, parent, depth);
            if truncated {
                break;
            }
        }
        w.path.pop();
        truncated
    }
}

impl Access for Ax {
    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        let apps = self.listed();
        Ok(self.app_frames(apps))
    }

    fn frames_of(&mut self, pid: u32) -> Result<Vec<Frame>, String> {
        let apps = self.listed().into_iter().filter(|(p, _)| *p as u32 == pid);
        Ok(self.app_frames(apps))
    }

    fn off_space(&mut self) -> Vec<Frame> {
        let own = std::process::id();
        let (name, owner, layer, shown) = (
            CFString::from_static_string("kCGWindowName"),
            CFString::from_static_string("kCGWindowOwnerName"),
            CFString::from_static_string("kCGWindowLayer"),
            CFString::from_static_string("kCGWindowIsOnscreen"),
        );
        let (id, pid) = (
            CFString::from_static_string("kCGWindowNumber"),
            CFString::from_static_string("kCGWindowOwnerPID"),
        );
        off_space_windows()
            .iter()
            .filter(|d| number(d, &layer) == Some(0) && number(d, &shown) != Some(1))
            .filter_map(|d| {
                let pid = u32::try_from(number(d, &pid)?).ok()?;
                let exe = || exe_path(i32::try_from(pid).ok()?);
                if pid == own || self.driver.is_some() && exe() == self.driver {
                    return None;
                }
                Some(Frame {
                    id: number(d, &id)?.to_string(),
                    title: string(d.find(&name).map(|v| (*v).clone())),
                    role: "window",
                    pid: Some(pid),
                    app: Some(string(d.find(&owner).map(|v| (*v).clone()))),
                    space: Some("other"),
                    ..Frame::default()
                })
            })
            .collect()
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let started = Instant::now();
        let deadline = started + WALK_BUDGET;
        let el = self.element(&frame.id)?;
        if string(attr(&el, "AXRole")).is_empty() {
            return Err(format!(
                "no accessibility answer within {} ms from window {:?}: it closed or its app is busy; take a new snapshot",
                MESSAGING_TIMEOUT.as_millis(),
                frame.title
            ));
        }
        let mut w = Walk {
            extras,
            scale: display_scale(),
            deadline,
            nodes: Vec::new(),
            path: Vec::new(),
        };
        let mut truncated = self.walk(&mut w, &el, None, 0);
        let walked = started.elapsed().as_micros();
        if frame.role == "window" && !truncated && extras.menus {
            let app = frame.pid.map(|p| application(p as i32));
            if let Some(bar) = app.as_ref().and_then(|a| attr(a, "AXMenuBar")) {
                let root = (!w.nodes.is_empty()).then_some(0);
                truncated = self.walk(&mut w, &bar, root, 1);
            }
        }
        let phases = vec![
            ("walk", walked),
            ("menubar", started.elapsed().as_micros() - walked),
        ];
        Ok(Tree {
            frame: frame.clone(),
            nodes: w.nodes,
            truncated,
            phases,
        })
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let el = self.element(id)?;
        let a = attrs(&self.names, &el);
        if a.role.is_empty() {
            return Err(format!("element {id} is gone"));
        }
        Ok(self.node_of(&el, &a, extras, chars, display_scale()))
    }

    fn keyboard_focus(&mut self, pid: u32) -> Result<Option<Node>, String> {
        let pid = i32::try_from(pid).map_err(|_| format!("no process {pid}"))?;
        let Some(el) = attr(&application(pid), "AXFocusedUIElement") else {
            return Ok(None);
        };
        let a = attrs(&self.names, &el);
        if a.role.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.node_of(
            &el,
            &a,
            Extras::FRAME,
            VALUE_CHARS,
            display_scale(),
        )))
    }

    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
        let mut chain = Vec::new();
        let mut at = self.element(id)?;
        while chain.len() < MAX_DEPTH {
            let Some(parent) = attr(&at, "AXParent") else {
                break;
            };
            if string(attr(&parent, "AXRole")) == "AXApplication" {
                break;
            }
            chain.push(self.id_of(&parent));
            at = parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String> {
        let w = self.element(window)?;
        let app = application(pid(&w).ok_or("the window has no process")?);
        Ok(hit(&app, x, y, display_scale()).map(|h| self.id_of(&h)))
    }

    fn hint(&mut self) -> Option<u64> {
        let mut h = DefaultHasher::new();
        let (owner, window, name) = (
            CFString::from_static_string("kCGWindowOwnerPID"),
            CFString::from_static_string("kCGWindowNumber"),
            CFString::from_static_string("kCGWindowName"),
        );
        for d in on_screen() {
            let title = string(d.find(&name).map(|v| (*v).clone()));
            (number(&d, &owner), number(&d, &window), title).hash(&mut h);
        }
        Some(h.finish())
    }

    fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
        Ok(pid(&self.element(id)?).and_then(|p| u32::try_from(p).ok()))
    }

    fn web_of(&mut self, id: &str) -> Result<(Option<String>, Option<String>), String> {
        let el = self.element(id)?;
        let dom_id = Some(string(attr(&el, "AXDOMIdentifier"))).filter(|d| !d.is_empty());
        Ok((attr(&el, "AXURL").and_then(url_text), dom_id))
    }

    fn web(&mut self, pid: u32, on: bool) -> Result<bool, String> {
        web(pid, on)
    }

    fn owner_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
        Ok(checked_hit(&system_wide(), x, y, display_scale())
            .map_err(|e| format!("accessibility hit test at ({x}, {y}) failed with {e}"))?
            .and_then(|h| pid(&h))
            .and_then(|p| u32::try_from(p).ok()))
    }

    fn listed_owner_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
        let scale = display_scale();
        let point = (f64::from(x) / scale, f64::from(y) / scale);
        let own = std::process::id();
        Ok(top_listed(&stacked(), point, |pid| {
            pid == own
                || self.driver.is_some()
                    && i32::try_from(pid).is_ok_and(|p| exe_path(p) == self.driver)
        }))
    }

    fn pid_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
        Ok(hit(&system_wide(), x, y, display_scale())
            .and_then(|h| pid(&h))
            .and_then(|p| u32::try_from(p).ok()))
    }

    fn covering(
        &mut self,
        frame: &Frame,
        x: i32,
        y: i32,
    ) -> Result<Option<(String, Option<u32>)>, String> {
        let Some(mut at) = hit(&system_wide(), x, y, display_scale()) else {
            return Ok(None);
        };
        let target = self.element(&frame.id)?;
        let mut window = None;
        for _ in 0..MAX_DEPTH {
            if at == target {
                return Ok(None);
            }
            if window.is_none() && string(attr(&at, "AXRole")) == "AXWindow" {
                window = Some(at.clone());
            }
            let Some(parent) = attr(&at, "AXParent") else {
                break;
            };
            at = parent;
        }
        Ok(window.map(|w| {
            let pid = pid(&w).and_then(|p| u32::try_from(p).ok());
            (string(attr(&w, "AXTitle")), pid)
        }))
    }

    fn menu_at(&mut self, x: i32, y: i32) -> Result<Option<String>, String> {
        Ok(hit(&system_wide(), x, y, display_scale())
            .filter(|h| string(attr(h, "AXRole")) == "AXMenuItem")
            .map(|h| self.id_of(&h)))
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let el = self.element(id)?;
        Ok(match act {
            Act::Action => pressed(&el) || perform(&el, "AXConfirm") || perform(&el, "AXPick"),
            Act::Select if string(attr(&el, "AXRole")) == "AXRadioButton" => {
                perform(&el, "AXPress")
            }
            Act::Select => set_true(&el, "AXSelected"),
            Act::Focus => set_true(&el, "AXFocused"),
        })
    }

    fn close(&mut self, frame: &Frame) -> Result<(), String> {
        let w = self.element(&frame.id)?;
        let button = attr(&w, "AXCloseButton").ok_or("the window has no close button")?;
        if perform(&button, "AXPress") {
            Ok(())
        } else {
            Err("the close button did not respond".to_owned())
        }
    }
}

struct Walk {
    extras: Extras,
    scale: f64,
    deadline: Instant,
    nodes: Vec<Node>,
    path: Vec<CFType>,
}

/// One element's attributes from a batched read; an attribute it lacks stays empty.
#[derive(Default)]
struct Attrs {
    role: String,
    subrole: String,
    title: String,
    description: String,
    help: String,
    value: Option<CFType>,
    enabled: Option<bool>,
    focused: bool,
    selected: bool,
    expanded: Option<bool>,
    pos: Option<(f64, f64)>,
    size: Option<(f64, f64)>,
    children: Vec<CFType>,
    cmd_char: String,
    cmd_modifiers: Option<i64>,
    mark: String,
    modal: bool,
}

#[repr(C)]
#[derive(Default)]
struct Pair {
    a: f64,
    b: f64,
}

struct Cgs {
    main_connection: unsafe extern "C" fn() -> i32,
    managed_spaces: unsafe extern "C" fn(i32) -> CFArrayRef,
    windows: unsafe extern "C" fn(i32, u32, CFArrayRef, u32, *mut u64, *mut u64) -> CFArrayRef,
}

impl Cgs {
    fn get() -> Option<&'static Self> {
        CGS.get_or_init(|| {
            Some(Self {
                // SAFETY: the symbol is CGSMainConnectionID, which has this signature.
                main_connection: unsafe {
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(symbol(
                        c"CGSMainConnectionID",
                    )?)
                },
                // SAFETY: the symbol is CGSCopyManagedDisplaySpaces, which has this signature.
                managed_spaces: unsafe {
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn(i32) -> CFArrayRef>(
                        symbol(c"CGSCopyManagedDisplaySpaces")?,
                    )
                },
                // SAFETY: the symbol is CGSCopyWindowsWithOptionsAndTags, which has this signature.
                windows: unsafe {
                    std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(
                            i32,
                            u32,
                            CFArrayRef,
                            u32,
                            *mut u64,
                            *mut u64,
                        ) -> CFArrayRef,
                    >(symbol(c"CGSCopyWindowsWithOptionsAndTags")?)
                },
            })
        })
        .as_ref()
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(e: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(
        e: CFTypeRef,
        attrs: CFArrayRef,
        options: u32,
        out: *mut CFArrayRef,
    ) -> i32;
    fn AXUIElementPerformAction(e: CFTypeRef, action: CFStringRef) -> i32;
    fn AXUIElementSetAttributeValue(e: CFTypeRef, attr: CFStringRef, value: CFTypeRef) -> i32;
    fn AXUIElementCopyElementAtPosition(app: CFTypeRef, x: f32, y: f32, out: *mut CFTypeRef)
    -> i32;
    fn AXUIElementGetPid(e: CFTypeRef, pid: *mut i32) -> i32;
    fn _AXUIElementGetWindow(e: CFTypeRef, window: *mut u32) -> i32;
    fn AXUIElementSetMessagingTimeout(e: CFTypeRef, seconds: f32) -> i32;
    fn AXValueGetValue(v: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
}

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGWindowListCreateDescriptionFromArray(windows: CFArrayRef) -> CFArrayRef;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut c_void);
}

fn hung(slow: Duration, listed: Duration) -> bool {
    slow >= HUNG_AFTER && listed >= LAUNCH_GRACE
}

fn attrs(names: &CFArray<CFString>, el: &CFType) -> Attrs {
    let [
        role,
        subrole,
        title,
        description,
        help,
        value,
        enabled,
        focused,
        selected,
        expanded,
        pos,
        size,
        children,
        cmd_char,
        cmd_modifiers,
        mark,
        modal,
    ] = values(names, el);
    Attrs {
        role: string(role),
        subrole: string(subrole),
        title: string(title),
        description: string(description),
        help: string(help),
        value,
        enabled: boolean(enabled),
        focused: boolean(focused).unwrap_or(false),
        selected: boolean(selected).unwrap_or(false),
        expanded: boolean(expanded),
        pos: pos.and_then(|v| pair(&v, POINT_TYPE)),
        size: size.and_then(|v| pair(&v, SIZE_TYPE)),
        children: children.map(|v| items(&v)).unwrap_or_default(),
        cmd_char: string(cmd_char),
        cmd_modifiers: cmd_modifiers
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64()),
        mark: string(mark),
        modal: boolean(modal).unwrap_or(false),
    }
}

fn values<const N: usize>(names: &CFArray<CFString>, el: &CFType) -> [Option<CFType>; N] {
    let mut out: CFArrayRef = std::ptr::null();
    // SAFETY: `el` is a live AXUIElement, `names` a live CFArray of CFStrings, `out` a valid out pointer.
    let err = unsafe {
        AXUIElementCopyMultipleAttributeValues(
            el.as_CFTypeRef(),
            names.as_concrete_TypeRef(),
            0,
            &mut out,
        )
    };
    if err != AX_SUCCESS || out.is_null() {
        return std::array::from_fn(|_| None);
    }
    // SAFETY: a successful copy returns the array at +1, wrapped once under the create rule.
    let got: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(out) };
    std::array::from_fn(|i| got.get(i as isize).map(|v| (*v).clone()).filter(is_value))
}

fn names<const N: usize>(attrs: [&'static str; N]) -> CFArray<CFString> {
    CFArray::from_CFTypes(&attrs.map(CFString::from_static_string))
}

fn string(v: Option<CFType>) -> String {
    v.and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn url_text(v: CFType) -> Option<String> {
    v.downcast::<CFURL>().map(|u| u.get_string().to_string())
}

fn boolean(v: Option<CFType>) -> Option<bool> {
    v.and_then(|v| v.downcast::<CFBoolean>()).map(bool::from)
}

fn sheets(children: Vec<CFType>) -> Vec<(CFType, Option<usize>)> {
    let sheet = |c: &CFType| string(attr(c, "AXRole")) == "AXSheet";
    let mut out = Vec::new();
    let mut level: Vec<(CFType, Option<usize>)> = children
        .into_iter()
        .filter(sheet)
        .map(|c| (c, None))
        .collect();
    while !level.is_empty() {
        let mut next = Vec::new();
        for (s, parent) in level {
            let at = Some(out.len());
            let kids = attr(&s, "AXChildren")
                .map(|c| items(&c))
                .unwrap_or_default();
            next.extend(kids.into_iter().filter(sheet).map(|c| (c, at)));
            out.push((s, parent));
        }
        level = next;
    }
    out
}

fn items(v: &CFType) -> Vec<CFType> {
    let Some(list) = v.downcast::<CFArray>() else {
        return Vec::new();
    };
    list.iter()
        // SAFETY: each item of a live CFArray is a live CF object; the get rule retains it.
        .map(|item| unsafe { CFType::wrap_under_get_rule(*item) })
        .collect()
}

fn is_value(v: &CFType) -> bool {
    // SAFETY: both calls only read the type ids of live CF objects.
    let (kind, ax_value) = unsafe { (CFGetTypeID(v.as_CFTypeRef()), AXValueGetTypeID()) };
    kind != ax_value || pair(v, POINT_TYPE).is_some() || pair(v, SIZE_TYPE).is_some()
}

fn pair(v: &CFType, kind: u32) -> Option<(f64, f64)> {
    // SAFETY: only reads the type ids of live CF objects.
    if unsafe { CFGetTypeID(v.as_CFTypeRef()) != AXValueGetTypeID() } {
        return None;
    }
    let mut p = Pair::default();
    // SAFETY: `v` is a live AXValue and `p` matches the layout of CGPoint and CGSize, two f64 fields.
    let ok = unsafe { AXValueGetValue(v.as_CFTypeRef(), kind, (&raw mut p).cast()) };
    ok.then_some((p.a, p.b))
}

fn hit(el: &CFType, x: i32, y: i32, scale: f64) -> Option<CFType> {
    checked_hit(el, x, y, scale).ok().flatten()
}

/// The element at `(x, y)`; none when nothing is there, the AX error code when the hit test fails.
fn checked_hit(el: &CFType, x: i32, y: i32, scale: f64) -> Result<Option<CFType>, i32> {
    let mut hit: CFTypeRef = std::ptr::null();
    let (px, py) = (f64::from(x) / scale, f64::from(y) / scale);
    // SAFETY: `el` is a live AXUIElement and `hit` a valid out pointer for one CF object.
    let err = unsafe {
        AXUIElementCopyElementAtPosition(el.as_CFTypeRef(), px as f32, py as f32, &mut hit)
    };
    match err {
        AX_SUCCESS if !hit.is_null() => {
            // SAFETY: a successful copy returns the element at +1, wrapped once under the create rule.
            Ok(Some(unsafe { CFType::wrap_under_create_rule(hit) }))
        }
        AX_SUCCESS | AX_NO_VALUE => Ok(None),
        e => Err(e),
    }
}

fn attr(el: &CFType, name: &'static str) -> Option<CFType> {
    let name = CFString::from_static_string(name);
    let mut out: CFTypeRef = std::ptr::null();
    // SAFETY: `el` is a live AXUIElement, `name` a live CFString and `out` a valid out pointer.
    let err = unsafe {
        AXUIElementCopyAttributeValue(el.as_CFTypeRef(), name.as_concrete_TypeRef(), &mut out)
    };
    if err != AX_SUCCESS || out.is_null() {
        return None;
    }
    // SAFETY: a successful copy returns the value at +1, wrapped once under the create rule.
    Some(unsafe { CFType::wrap_under_create_rule(out) })
}

fn perform(el: &CFType, action: &'static str) -> bool {
    perform_code(el, action) == AX_SUCCESS
}

/// Whether `el` took its press; an app that does not answer in time, as one in the modal loop the press opened, has taken it.
fn pressed(el: &CFType) -> bool {
    matches!(perform_code(el, "AXPress"), AX_SUCCESS | AX_CANNOT_COMPLETE)
}

fn perform_code(el: &CFType, action: &'static str) -> i32 {
    let action = CFString::from_static_string(action);
    // SAFETY: `el` is a live AXUIElement and `action` a live CFString.
    unsafe { AXUIElementPerformAction(el.as_CFTypeRef(), action.as_concrete_TypeRef()) }
}

fn set_true(el: &CFType, name: &'static str) -> bool {
    set_bool(el, name, true)
}

fn set_bool(el: &CFType, name: &'static str, on: bool) -> bool {
    let name = CFString::from_static_string(name);
    let value = CFBoolean::from(on);
    // SAFETY: `el` is a live AXUIElement; `name` and `value` are live CF objects.
    unsafe {
        AXUIElementSetAttributeValue(
            el.as_CFTypeRef(),
            name.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
        ) == AX_SUCCESS
    }
}

fn pid(el: &CFType) -> Option<i32> {
    // SAFETY: only reads the type id of a live CF object.
    if unsafe { CFGetTypeID(el.as_CFTypeRef()) != AXUIElementGetTypeID() } {
        return None;
    }
    let mut p = 0;
    // SAFETY: `el` is a live AXUIElement and `p` a valid out pointer.
    (unsafe { AXUIElementGetPid(el.as_CFTypeRef(), &mut p) } == AX_SUCCESS).then_some(p)
}

fn number_of(window: &CFType) -> Option<u32> {
    pid(window)?;
    let mut n = 0;
    // SAFETY: `window` is a live AXUIElement and `n` a valid out pointer.
    (unsafe { _AXUIElementGetWindow(window.as_CFTypeRef(), &mut n) } == AX_SUCCESS && n != 0)
        .then_some(n)
}

fn application(p: i32) -> CFType {
    // SAFETY: the application element is returned at +1 and wrapped once under the create rule.
    let app = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(p)) };
    set_timeout(&app);
    app
}

/// An element a hit test found: its role and name as a snapshot names them, its help, its process, and the role and name of each ancestor up to its window, innermost first.
pub struct Seen {
    pub role: &'static str,
    pub name: String,
    pub description: String,
    pub pid: Option<u32>,
    pub path: Vec<(&'static str, String)>,
    /// A secure text field: a password, whose keys no listener sees.
    pub secret: bool,
}

/// What the hit test finds at desk point `(x, y)` now, read in one call per element so a click can be read before its app handles it.
pub fn seen_at(x: i32, y: i32) -> Option<Seen> {
    let names = names(ATTRS);
    let el = hit(&system_wide(), x, y, display_scale())?;
    let a = attrs(&names, &el);
    let mut path = Vec::new();
    let mut up = attr(&el, "AXParent");
    while let Some(p) = up.take() {
        let pa = attrs(&names, &p);
        path.push((
            ax_roles::role(kind_of(&pa)).unwrap_or("generic"),
            name_of(&pa, NAME_CHARS),
        ));
        if pa.role == "AXWindow" || path.len() >= PATH_MAX_DEPTH {
            break;
        }
        up = attr(&p, "AXParent");
    }
    Some(Seen {
        role: ax_roles::role(kind_of(&a)).unwrap_or("generic"),
        name: name_of(&a, NAME_CHARS),
        description: a.help,
        pid: pid(&el).and_then(|p| u32::try_from(p).ok()),
        path,
        secret: [&a.role, &a.subrole]
            .iter()
            .any(|r| *r == "AXSecureTextField"),
    })
}

fn kind_of(a: &Attrs) -> Kind<'_> {
    Kind {
        role: &a.role,
        subrole: &a.subrole,
        named: !a.title.is_empty() || !a.description.is_empty(),
        submenu: a.role == "AXMenuItem" && !a.children.is_empty(),
        marked: !a.mark.is_empty(),
    }
}

fn name_of(a: &Attrs, chars: usize) -> String {
    let static_text = (a.role == "AXStaticText")
        .then(|| a.value.as_ref().map(|v| text(v, chars)))
        .flatten()
        .unwrap_or_default();
    [&a.title, &a.description, &static_text]
        .into_iter()
        .find(|n| !n.is_empty())
        .map_or("", |n| n.as_str())
        .trim_end()
        .to_owned()
}

/// Whether the session's secure input is on: a password field has the keyboard focus.
pub fn secure_input() -> bool {
    // SAFETY: the call only reads the session's secure input state.
    unsafe { IsSecureEventInputEnabled() }
}

/// Where typing goes now: the process of the focused element, and whether it is a password field, as a secure text field or under the session's secure input.
pub fn typing() -> (Option<u32>, bool) {
    typing_at(attr(&system_wide(), "AXFocusedUIElement"))
}

/// The focused element as a snapshot names it, the role of its window or sheet, the title of its window, and whether it is a password field.
pub struct Focused {
    pub role: &'static str,
    pub name: String,
    pub frame: &'static str,
    pub window: String,
    pub secure: bool,
}

pub fn focused() -> Option<Focused> {
    let names = names(ATTRS);
    let el = attr(&system_wide(), "AXFocusedUIElement")?;
    let a = attrs(&names, &el);
    let mut frame = None;
    let mut window = String::new();
    let mut up = attr(&el, "AXParent");
    for _ in 0..PATH_MAX_DEPTH {
        let Some(p) = up.take() else { break };
        let role = string(attr(&p, "AXRole"));
        if role == "AXSheet" {
            frame.get_or_insert("dialog");
        }
        if role == "AXWindow" {
            let subrole = string(attr(&p, "AXSubrole"));
            frame.get_or_insert(
                ax_roles::role(Kind {
                    role: &role,
                    subrole: &subrole,
                    ..Kind::default()
                })
                .unwrap_or("window"),
            );
            window = string(attr(&p, "AXTitle"));
            break;
        }
        up = attr(&p, "AXParent");
    }
    Some(Focused {
        role: ax_roles::role(kind_of(&a)).unwrap_or("generic"),
        name: name_of(&a, NAME_CHARS),
        frame: frame.unwrap_or("window"),
        window,
        secure: [&a.role, &a.subrole]
            .iter()
            .any(|r| *r == "AXSecureTextField"),
    })
}

/// Whether process `pid` is a Chromium or Electron app, whose bundle holds a framework with helper apps.
pub fn chromium(pid: u32) -> bool {
    let Some(exe) = i32::try_from(pid).ok().and_then(exe_path) else {
        return false;
    };
    let Some(contents) = exe.ancestors().find(|a| a.ends_with("Contents")) else {
        return false;
    };
    std::fs::read_dir(contents.join("Frameworks")).is_ok_and(|dir| {
        dir.flatten().any(|f| {
            f.path().extension().is_some_and(|e| e == "framework")
                && f.path().join("Helpers").is_dir()
        })
    })
}

/// Turns the web accessibility tree of Chromium app `pid` on or off and answers whether it was on before.
pub fn web(pid: u32, on: bool) -> Result<bool, String> {
    if !chromium(pid) {
        return Err(format!("process {pid} is no Chromium app"));
    }
    let app = application(i32::try_from(pid).map_err(|_| format!("no process {pid}"))?);
    for switch in WEB_SWITCHES {
        let Some(was) = boolean(attr(&app, switch)) else {
            continue;
        };
        // chrome answers the set with kAXErrorNotImplemented and applies it, so the read back decides
        if was != on {
            set_bool(&app, switch, on);
        }
        if boolean(attr(&app, switch)) == Some(on) {
            return Ok(was);
        }
    }
    Err(format!("process {pid} takes no web accessibility switch"))
}

fn typing_at(focused: Option<CFType>) -> (Option<u32>, bool) {
    let pid = focused
        .as_ref()
        .and_then(pid)
        .and_then(|p| u32::try_from(p).ok());
    let secure = secure_input();
    let field = focused.is_some_and(|f| {
        [attr(&f, "AXSubrole"), attr(&f, "AXRole")]
            .into_iter()
            .any(|a| string(a) == "AXSecureTextField")
    });
    (pid, secure || field)
}

fn system_wide() -> CFType {
    // SAFETY: the system-wide element is returned at +1 and wrapped once under the create rule.
    unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) }
}

fn set_timeout(el: &CFType) {
    // SAFETY: `el` is a live AXUIElement for the duration of the call.
    unsafe { AXUIElementSetMessagingTimeout(el.as_CFTypeRef(), MESSAGING_TIMEOUT.as_secs_f32()) };
}

fn trusted(prompt: bool) -> bool {
    let options = CFDictionary::from_CFType_pairs(&[(
        CFString::from_static_string("AXTrustedCheckOptionPrompt"),
        CFBoolean::from(prompt).as_CFType(),
    )]);
    // SAFETY: `options` is a live CFDictionary; the call reads this process's TCC state and, once, asks the user.
    unsafe { AXIsProcessTrustedWithOptions(options.as_CFTypeRef()) }
}

fn hash_of(el: &CFType) -> usize {
    // SAFETY: `el` is a live CF object for the duration of the call.
    unsafe { CFHash(el.as_CFTypeRef()) }
}

fn exe_path(pid: i32) -> Option<PathBuf> {
    let mut buf = [0u8; PATH_MAX];
    // SAFETY: proc_pidpath writes at most `buf.len()` bytes into `buf` and returns the count written.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), PATH_MAX as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n])))
}

/// The processes with a window on screen, front to back, each with the bounds of those windows.
fn shown_windows() -> Vec<(i32, Vec<[f64; 4]>)> {
    let mut apps: Vec<(i32, Vec<[f64; 4]>)> = Vec::new();
    let (owner, bounds) = (
        CFString::from_static_string("kCGWindowOwnerPID"),
        CFString::from_static_string("kCGWindowBounds"),
    );
    let sides = ["X", "Y", "Width", "Height"].map(CFString::from_static_string);
    for d in on_screen() {
        let Some(p) = number(&d, &owner).and_then(|p| i32::try_from(p).ok()) else {
            continue;
        };
        let rect = d.find(&bounds).and_then(|v| dictionary(&v)).and_then(|b| {
            let [x, y, w, h] = sides.each_ref().map(|k| float(&b, k));
            Some([x?, y?, w?, h?])
        });
        let at = apps.iter().position(|(q, _)| *q == p).unwrap_or_else(|| {
            apps.push((p, Vec::new()));
            apps.len() - 1
        });
        apps[at].1.extend(rect);
    }
    apps
}

/// Whether AX window frame `frame` is one of `bounds`, the on-screen windows of its process; an unread frame counts as shown.
fn shows(bounds: &[[f64; 4]], frame: Option<[f64; 4]>) -> bool {
    frame.is_none_or(|f| {
        bounds
            .iter()
            .any(|b| b.iter().zip(f).all(|(a, f)| (a - f).abs() <= 1.0))
    })
}

/// An on-screen window as the window server lists it: process, level, alpha and bounds in points.
struct Listed {
    pid: u32,
    layer: i64,
    alpha: f64,
    bounds: [f64; 4],
}

/// The windows on screen on every level, front to back.
fn stacked() -> Vec<Listed> {
    let (owner, layer, alpha, bounds) = (
        CFString::from_static_string("kCGWindowOwnerPID"),
        CFString::from_static_string("kCGWindowLayer"),
        CFString::from_static_string("kCGWindowAlpha"),
        CFString::from_static_string("kCGWindowBounds"),
    );
    let sides = ["X", "Y", "Width", "Height"].map(CFString::from_static_string);
    window_list()
        .iter()
        .filter_map(|d| {
            let b = d.find(&bounds).and_then(|v| dictionary(&v))?;
            let [x, y, w, h] = sides.each_ref().map(|k| float(&b, k));
            Some(Listed {
                pid: u32::try_from(number(d, &owner)?).ok()?,
                layer: number(d, &layer)?,
                alpha: float(d, &alpha).unwrap_or(1.0),
                bounds: [x?, y?, w?, h?],
            })
        })
        .collect()
}

/// The process of the first window in `windows` (front to back) that holds `point` and shows to the user: a visible window below the overlay levels, not one of `skip`'s.
fn top_listed(windows: &[Listed], (px, py): (f64, f64), skip: impl Fn(u32) -> bool) -> Option<u32> {
    windows
        .iter()
        .filter(|w| (0..OVERLAY_LEVEL).contains(&w.layer) && w.alpha > 0.0)
        .filter(|w| {
            let [x, y, width, height] = w.bounds;
            (x..x + width).contains(&px) && (y..y + height).contains(&py)
        })
        .find(|w| !skip(w.pid))
        .map(|w| w.pid)
}

/// The layer-0 windows on screen, front to back.
fn on_screen() -> Vec<CFDictionary<CFString, CFType>> {
    let layer = CFString::from_static_string("kCGWindowLayer");
    let mut list = window_list();
    list.retain(|d| number(d, &layer) == Some(0));
    list
}

fn window_list() -> Vec<CFDictionary<CFString, CFType>> {
    // SAFETY: the window list is returned at +1, or null, and wrapped once under the create rule.
    let list = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: `list` is a non-null CFArray at +1.
    let list: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(list) };
    list.iter().map(|d| (*d).clone()).collect()
}

fn off_space_windows() -> Vec<CFDictionary<CFString, CFType>> {
    let Some(cgs) = Cgs::get() else {
        return Vec::new();
    };
    let (current, spaces, managed, kind) = (
        CFString::from_static_string("Current Space"),
        CFString::from_static_string("Spaces"),
        CFString::from_static_string("ManagedSpaceID"),
        CFString::from_static_string("type"),
    );
    // SAFETY: the connection id is a plain integer, and the display list comes at +1 or null, wrapped once under the create rule.
    let (cid, displays) = unsafe {
        let cid = (cgs.main_connection)();
        (cid, (cgs.managed_spaces)(cid))
    };
    if displays.is_null() {
        return Vec::new();
    }
    // SAFETY: `displays` is a non-null CFArray at +1.
    let displays: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(displays) };
    let mut other = Vec::new();
    for display in displays.iter().filter_map(|d| dictionary(&d)) {
        let shown = display
            .find(&current)
            .and_then(|c| dictionary(&c))
            .and_then(|c| number(&c, &managed));
        let listed = display.find(&spaces).map(|s| items(&s)).unwrap_or_default();
        other.extend(
            listed
                .iter()
                .filter_map(dictionary)
                .filter(|s| user_space(number(s, &kind)))
                .filter_map(|s| number(&s, &managed))
                .filter(|&id| Some(id) != shown)
                .map(CFNumber::from),
        );
    }
    if other.is_empty() {
        return Vec::new();
    }
    let other = CFArray::from_CFTypes(&other);
    let (mut set_tags, mut clear_tags) = (0, 0);
    // SAFETY: `other` is a live CFArray of space ids and the tag pointers are valid; the window list comes at +1 or null.
    let windows = unsafe {
        (cgs.windows)(
            cid,
            0,
            other.as_concrete_TypeRef(),
            2,
            &raw mut set_tags,
            &raw mut clear_tags,
        )
    };
    if windows.is_null() {
        return Vec::new();
    }
    // SAFETY: `windows` is a non-null CFArray of CFNumbers at +1.
    let windows: CFArray<CFNumber> = unsafe { CFArray::wrap_under_create_rule(windows) };
    let ids: Vec<*const c_void> = windows
        .iter()
        .filter_map(|n| n.to_i64())
        .map(|id| id as usize as *const c_void)
        .collect();
    // SAFETY: the description call takes a CFArray of raw window ids with no callbacks; the array is released after it, and the descriptions come at +1 or null.
    let described = unsafe {
        let raw = CFArrayCreate(
            std::ptr::null(),
            ids.as_ptr(),
            ids.len() as isize,
            std::ptr::null(),
        );
        let described = CGWindowListCreateDescriptionFromArray(raw);
        core_foundation::base::CFRelease(raw.cast());
        described
    };
    if described.is_null() {
        return Vec::new();
    }
    // SAFETY: `described` is a non-null CFArray of window dictionaries at +1.
    let described: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(described) };
    described.iter().map(|d| (*d).clone()).collect()
}

fn user_space(kind: Option<i64>) -> bool {
    matches!(kind, Some(0 | 4))
}

fn symbol(name: &CStr) -> Option<*mut c_void> {
    // SAFETY: `name` is a NUL-terminated symbol name; RTLD_DEFAULT searches the loaded images.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

fn dictionary(v: &CFType) -> Option<CFDictionary<CFString, CFType>> {
    let d = v.downcast::<CFDictionary>()?;
    // SAFETY: `d` is a live CFDictionary whose keys are CFStrings; the get rule retains it.
    Some(unsafe { CFDictionary::wrap_under_get_rule(d.as_concrete_TypeRef()) })
}

fn number(d: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
    d.find(key)
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i64())
}

fn float(d: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    d.find(key)
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_f64())
}

/// The main display's backing scale: desk coordinates per point.
pub fn display_scale() -> f64 {
    // SAFETY: the main display id is always valid; the mode is released below when non-null.
    unsafe {
        let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
        if mode.is_null() {
            return 1.0;
        }
        let (px, pt) = (
            CGDisplayModeGetPixelWidth(mode),
            CGDisplayModeGetWidth(mode),
        );
        CGDisplayModeRelease(mode);
        if pt == 0 { 1.0 } else { px as f64 / pt as f64 }
    }
}

fn text(v: &CFType, chars: usize) -> String {
    if let Some(s) = v.downcast::<CFString>() {
        return s.to_string().chars().take(chars).collect();
    }
    v.downcast::<CFNumber>()
        .and_then(|n| n.to_f64())
        .map(format_number)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_shows_when_it_is_one_of_its_process_s_on_screen_windows() {
        let main = [-1416.0, 0.0, 846.0, 774.0];
        assert!(shows(&[main], Some([-1416.4, 0.0, 846.0, 774.6])));
        assert!(!shows(&[main], Some([0.0, 0.0, 280.0, 380.0])));
        assert!(!shows(&[], Some(main)));
        assert!(shows(&[], None));
    }

    fn listed(pid: u32, layer: i64, alpha: f64, bounds: [f64; 4]) -> Listed {
        Listed {
            pid,
            layer,
            alpha,
            bounds,
        }
    }

    #[test]
    fn the_top_listed_window_at_a_point_is_the_first_visible_one_below_the_overlays() {
        let windows = [
            listed(5, 2000, 1.0, [0.0, 0.0, 1280.0, 800.0]),
            listed(6, 3, 0.0, [0.0, 0.0, 1280.0, 800.0]),
            listed(7, 25, 1.0, [0.0, 0.0, 1280.0, 30.0]),
            listed(8, 3, 1.0, [400.0, 300.0, 200.0, 100.0]),
            listed(9, 0, 1.0, [100.0, 100.0, 800.0, 600.0]),
        ];
        let none = |_| false;
        assert_eq!(top_listed(&windows, (500.0, 350.0), none), Some(8));
        assert_eq!(top_listed(&windows, (200.0, 200.0), none), Some(9));
        assert_eq!(top_listed(&windows, (200.0, 10.0), none), Some(7));
        assert_eq!(top_listed(&windows, (1000.0, 750.0), none), None);
        assert_eq!(top_listed(&windows, (500.0, 350.0), |p| p == 8), Some(9));
    }

    #[test]
    fn an_app_listed_within_the_launch_grace_is_not_skipped_as_hung() {
        let s = Duration::from_secs;
        assert!(!hung(s(1), s(30)));
        assert!(hung(s(2), s(30)));
        assert!(!hung(s(5), s(4)));
        assert!(hung(s(5), s(10)));
    }

    #[test]
    fn a_process_path_is_its_canonical_executable_as_the_daemon_finds_its_driver_beside() {
        let own = i32::try_from(std::process::id()).unwrap();
        assert_eq!(
            exe_path(own),
            std::env::current_exe().and_then(std::fs::canonicalize).ok()
        );
        assert_eq!(exe_path(-1), None);
    }

    #[test]
    fn desktop_and_full_screen_spaces_hold_app_windows_and_others_do_not() {
        assert!(user_space(Some(0)) && user_space(Some(4)));
        assert!(!user_space(Some(2)) && !user_space(None));
    }

    #[test]
    fn the_space_calls_resolve_at_run_time_and_a_missing_symbol_is_none() {
        assert!(Cgs::get().is_some());
        assert!(symbol(c"CGSNoSuchCallForShuttle").is_none());
    }
}
