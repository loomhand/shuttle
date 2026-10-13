//! The macOS desk backend: window trees by a walk over AX with one batched attribute read per element,
//! within a node and time budget; the app's menu bar joins its main window's tree.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFHash, CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

use crate::ax_roles::{self, Facts, Kind};
use crate::keys::ax_combo;
use crate::model::{Access, Act, Extras, Frame, Node, Tree, is_active, is_field};

const MAX_NODES: usize = 2500;
const MAX_DEPTH: usize = 64;
const WALK_BUDGET: Duration = Duration::from_secs(4);
const MESSAGING_TIMEOUT: f32 = 3.0;
const MAX_IDS: usize = 50_000;
const VALUE_CHARS: usize = 200;
const AX_SUCCESS: i32 = 0;

static PROMPTED: AtomicBool = AtomicBool::new(false);
const POINT_TYPE: u32 = 1;
const SIZE_TYPE: u32 = 2;
const ON_SCREEN_ONLY: u32 = 1;
const EXCLUDE_DESKTOP: u32 = 1 << 4;
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

#[repr(C)]
#[derive(Default)]
struct Pair {
    a: f64,
    b: f64,
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
    fn AXUIElementSetMessagingTimeout(e: CFTypeRef, seconds: f32) -> i32;
    fn AXValueGetValue(v: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut c_void);
}

/// The AX backend: element ids are `pid/n`, kept while this backend lives.
pub struct Ax {
    system: CFType,
    scale: f64,
    ids: HashMap<String, CFType>,
    hashes: HashMap<usize, Vec<(CFType, String)>>,
    next: u64,
}

impl Ax {
    /// The backend, once this process holds the Accessibility permission.
    pub fn open() -> Result<Self, String> {
        let prompt = !PROMPTED.swap(true, Ordering::Relaxed);
        let options = CFDictionary::from_CFType_pairs(&[(
            CFString::from_static_string("AXTrustedCheckOptionPrompt"),
            CFBoolean::from(prompt).as_CFType(),
        )]);
        // SAFETY: `options` is a live CFDictionary; the call reads this process's TCC state and, once, asks the user.
        if !unsafe { AXIsProcessTrustedWithOptions(options.as_CFTypeRef()) } {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Err(format!(
                "no Accessibility permission for {exe}: allow it in System Settings > Privacy & Security > Accessibility"
            ));
        }
        // SAFETY: the system-wide element is returned at +1 and wrapped once under the create rule.
        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        // SAFETY: `system` is a live AXUIElement for the duration of the call.
        unsafe { AXUIElementSetMessagingTimeout(system.as_CFTypeRef(), MESSAGING_TIMEOUT) };
        Ok(Self {
            system,
            scale: display_scale(),
            ids: HashMap::new(),
            hashes: HashMap::new(),
            next: 0,
        })
    }

    fn id_of(&mut self, el: &CFType) -> String {
        // SAFETY: `el` is a live CF object for the duration of the call.
        let hash = unsafe { CFHash(el.as_CFTypeRef()) };
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
        self.ids
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown element id {id:?}: take a new snapshot"))
    }

    fn node_of(&mut self, el: &CFType, a: &Attrs, extras: Extras, chars: usize) -> Node {
        let named = !a.title.is_empty() || !a.description.is_empty();
        let kind = Kind {
            role: &a.role,
            subrole: &a.subrole,
            named,
            submenu: a.role == "AXMenuItem" && !a.children.is_empty(),
            marked: !a.mark.is_empty(),
        };
        let role = ax_roles::role(kind).unwrap_or("generic");
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
        };
        let name = if a.title.is_empty() {
            &a.description
        } else {
            &a.title
        };
        let bounds = match (a.pos, a.size) {
            (Some((x, y)), Some((w, h))) if extras.bounds && shown && is_active(role) => {
                Some([x, y, w, h].map(|v| (v * self.scale).round() as i32))
            }
            _ => None,
        };
        Node {
            id: self.id_of(el),
            role,
            native_role: ax_roles::role(kind).is_none().then(|| a.role.clone()),
            name: name.trim_end().to_owned(),
            description: a.help.clone(),
            states: ax_roles::states(role, facts),
            value: (extras.values && is_field(role))
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

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        el: &CFType,
        parent: Option<usize>,
        depth: usize,
        extras: Extras,
        deadline: Instant,
        out: &mut Vec<Node>,
        path: &mut Vec<CFType>,
    ) -> bool {
        if out.len() >= MAX_NODES || Instant::now() >= deadline {
            return true;
        }
        if depth > MAX_DEPTH || path.contains(el) {
            return false;
        }
        let a = attrs(el);
        let mut node = self.node_of(el, &a, extras, VALUE_CHARS);
        let (parent, depth) = if node.role == "generic" && parent.is_some() {
            (parent, depth)
        } else {
            node.parent = parent;
            node.depth = depth;
            out.push(node);
            (Some(out.len() - 1), depth + 1)
        };
        path.push(el.clone());
        let mut truncated = false;
        for child in &a.children {
            truncated |= self.walk(child, parent, depth, extras, deadline, out, path);
            if truncated {
                break;
            }
        }
        path.pop();
        truncated
    }
}

impl Access for Ax {
    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        let front = attr(&self.system, "AXFocusedApplication").and_then(|a| pid(&a));
        let mut frames = Vec::new();
        for p in window_pids() {
            let app = application(p);
            let focused = attr(&app, "AXFocusedWindow");
            let Some(windows) = attr(&app, "AXWindows").map(|w| items(&w)) else {
                continue;
            };
            for w in windows.iter() {
                let a = attrs(w);
                let mut active = front == Some(p) && focused.as_ref() == Some(w);
                let title = a.title.trim().to_owned();
                for sheet in a.children.iter().filter(|c| attrs(c).role == "AXSheet") {
                    let s = attrs(sheet);
                    frames.push(Frame {
                        id: self.id_of(sheet),
                        title: if s.title.is_empty() {
                            title.clone()
                        } else {
                            s.title
                        },
                        role: "dialog",
                        pid: Some(p as u32),
                        active,
                    });
                    active = false;
                }
                let kind = Kind {
                    role: &a.role,
                    subrole: &a.subrole,
                    ..Kind::default()
                };
                frames.push(Frame {
                    id: self.id_of(w),
                    title,
                    role: ax_roles::role(kind).unwrap_or("window"),
                    pid: Some(p as u32),
                    active,
                });
            }
        }
        Ok(frames)
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let started = Instant::now();
        let deadline = started + WALK_BUDGET;
        let el = self.element(&frame.id)?;
        let mut nodes = Vec::new();
        let mut truncated = self.walk(&el, None, 0, extras, deadline, &mut nodes, &mut Vec::new());
        let walked = started.elapsed().as_micros();
        if frame.role == "window" && !truncated {
            let app = frame.pid.map(|p| application(p as i32));
            if let Some(bar) = app.as_ref().and_then(|a| attr(a, "AXMenuBar")) {
                let root = (!nodes.is_empty()).then_some(0);
                truncated = self.walk(&bar, root, 1, extras, deadline, &mut nodes, &mut Vec::new());
            }
        }
        let phases = vec![
            ("walk", walked),
            ("menubar", started.elapsed().as_micros() - walked),
        ];
        Ok(Tree {
            frame: frame.clone(),
            nodes,
            truncated,
            phases,
        })
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let el = self.element(id)?;
        let a = attrs(&el);
        if a.role.is_empty() {
            return Err(format!("element {id} is gone"));
        }
        Ok(self.node_of(&el, &a, extras, chars))
    }

    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
        let mut chain = Vec::new();
        let mut at = self.element(id)?;
        while chain.len() < MAX_DEPTH {
            let Some(parent) = attr(&at, "AXParent") else {
                break;
            };
            if attrs(&parent).role == "AXApplication" {
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
        let mut hit: CFTypeRef = std::ptr::null();
        let (px, py) = (f64::from(x) / self.scale, f64::from(y) / self.scale);
        // SAFETY: `app` is a live AXUIElement and `hit` a valid out pointer for one CF object.
        let err = unsafe {
            AXUIElementCopyElementAtPosition(app.as_CFTypeRef(), px as f32, py as f32, &mut hit)
        };
        if err != AX_SUCCESS || hit.is_null() {
            return Ok(None);
        }
        // SAFETY: a successful copy returns the element at +1, wrapped once under the create rule.
        let hit = unsafe { CFType::wrap_under_create_rule(hit) };
        Ok(Some(self.id_of(&hit)))
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let el = self.element(id)?;
        let a = attrs(&el);
        Ok(match act {
            Act::Action => {
                perform(&el, "AXPress") || perform(&el, "AXConfirm") || perform(&el, "AXPick")
            }
            Act::Select if a.role == "AXRadioButton" => perform(&el, "AXPress"),
            Act::Select => set(&el, "AXSelected", CFBoolean::true_value().as_CFType()),
            Act::Focus => set(&el, "AXFocused", CFBoolean::true_value().as_CFType()),
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

fn attrs(el: &CFType) -> Attrs {
    let names: Vec<CFString> = ATTRS
        .iter()
        .map(|n| CFString::from_static_string(n))
        .collect();
    let names = CFArray::from_CFTypes(&names);
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
        return Attrs::default();
    }
    // SAFETY: a successful copy returns the array at +1, wrapped once under the create rule.
    let values: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(out) };
    let v: Vec<Option<CFType>> = values
        .iter()
        .map(|v| Some((*v).clone()).filter(is_value))
        .collect();
    let get = |i: usize| v.get(i).cloned().flatten();
    let string = |i: usize| {
        get(i)
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    let boolean = |i: usize| {
        get(i)
            .and_then(|v| v.downcast::<CFBoolean>())
            .map(bool::from)
    };
    Attrs {
        role: string(0),
        subrole: string(1),
        title: string(2),
        description: string(3),
        help: string(4),
        value: get(5),
        enabled: boolean(6),
        focused: boolean(7).unwrap_or(false),
        selected: boolean(8).unwrap_or(false),
        expanded: boolean(9),
        pos: get(10).and_then(|v| pair(&v, POINT_TYPE)),
        size: get(11).and_then(|v| pair(&v, SIZE_TYPE)),
        children: get(12).map(|v| items(&v)).unwrap_or_default(),
        cmd_char: string(13),
        cmd_modifiers: get(14)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64()),
        mark: string(15),
        modal: boolean(16).unwrap_or(false),
    }
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
    let action = CFString::from_static_string(action);
    // SAFETY: `el` is a live AXUIElement and `action` a live CFString.
    unsafe {
        AXUIElementPerformAction(el.as_CFTypeRef(), action.as_concrete_TypeRef()) == AX_SUCCESS
    }
}

fn set(el: &CFType, name: &'static str, value: CFType) -> bool {
    let name = CFString::from_static_string(name);
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

fn application(p: i32) -> CFType {
    // SAFETY: the application element is returned at +1 and wrapped once under the create rule.
    let app = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(p)) };
    // SAFETY: `app` is a live AXUIElement for the duration of the call.
    unsafe { AXUIElementSetMessagingTimeout(app.as_CFTypeRef(), MESSAGING_TIMEOUT) };
    app
}

fn window_pids() -> Vec<i32> {
    // SAFETY: the window list is returned at +1, or null, and wrapped once under the create rule.
    let list = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: `list` is a non-null CFArray at +1.
    let list: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(list) };
    let number = |d: &CFDictionary<CFString, CFType>, key: &'static str| {
        d.find(CFString::from_static_string(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
    };
    let mut pids = Vec::new();
    for d in list.iter() {
        if number(&d, "kCGWindowLayer") == Some(0)
            && let Some(p) = number(&d, "kCGWindowOwnerPID").and_then(|p| i32::try_from(p).ok())
            && !pids.contains(&p)
        {
            pids.push(p);
        }
    }
    pids
}

fn display_scale() -> f64 {
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
    match v.downcast::<CFNumber>() {
        Some(n) => match (n.to_i64(), n.to_f64()) {
            (Some(i), Some(f)) if f.fract() == 0.0 => i.to_string(),
            (_, Some(f)) => f.to_string(),
            _ => String::new(),
        },
        None => String::new(),
    }
}
