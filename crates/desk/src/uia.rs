//! The Windows desk backend: window trees from one UIA cache request over the control view, with the
//! Toggle, ExpandCollapse, SelectionItem, Value and Window properties cached; ids are UIA runtime ids.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4, VT_R8, VariantClear};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, PostMessageW, WM_CLOSE};

use crate::keys::{Os, uia_combo};
use crate::model::{Access, Act, Extras, Frame, Node, Tree, is_active, is_field};
use crate::uia_roles::{self, Facts, Kind};

const MAX_NODES: usize = 2500;
const MAX_DEPTH: usize = 64;
const MAX_IDS: usize = 50_000;
const VALUE_CHARS: usize = 200;
const CACHE_TRIES: u32 = 3;
const CACHE_RETRY: Duration = Duration::from_millis(40);
const WINDOW: i32 = 50032;
const MENU: i32 = 50009;
const LEAF: i32 = 3;
const PROPS: [UIA_PROPERTY_ID; 27] = [
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_HelpTextPropertyId,
    UIA_IsEnabledPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_AcceleratorKeyPropertyId,
    UIA_AccessKeyPropertyId,
    UIA_ProcessIdPropertyId,
    UIA_NativeWindowHandlePropertyId,
    UIA_RuntimeIdPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_ValueValuePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_RangeValueValuePropertyId,
    UIA_WindowIsModalPropertyId,
    UIA_IsDialogPropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_IsRangeValuePatternAvailablePropertyId,
    UIA_IsWindowPatternAvailablePropertyId,
];

/// The UIA backend of this thread.
pub struct Uia {
    auto: IUIAutomation,
    subtree: IUIAutomationCacheRequest,
    single: IUIAutomationCacheRequest,
    root: IUIAutomationElement,
    ids: HashMap<String, IUIAutomationElement>,
}

impl Uia {
    /// The backend on a COM multithreaded apartment, with per-monitor DPI awareness so bounds are pixels.
    pub fn open() -> Result<Self, String> {
        // SAFETY: COM and DPI setup take no pointers; a repeated call returns S_FALSE or an error that is ignored.
        let auto: IUIAutomation = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        }
        .map_err(|e| format!("start UI Automation: {e}"))?;
        let subtree = request(&auto, TreeScope_Subtree)?;
        let single = request(&auto, TreeScope_Element)?;
        // SAFETY: `auto` is a live IUIAutomation on this thread.
        let root = unsafe { auto.GetRootElement() }.map_err(|e| format!("UIA root: {e}"))?;
        Ok(Self {
            auto,
            subtree,
            single,
            root,
            ids: HashMap::new(),
        })
    }

    fn element(&self, id: &str) -> Result<IUIAutomationElement, String> {
        self.ids
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown element id {id:?}: take a new snapshot"))
    }

    fn remember(&mut self, id: &str, el: &IUIAutomationElement) {
        if self.ids.len() >= MAX_IDS {
            self.ids.clear();
        }
        self.ids.entry(id.to_owned()).or_insert_with(|| el.clone());
    }

    fn id_of(&mut self, el: &IUIAutomationElement) -> Result<String, String> {
        let id = runtime_id(el)?;
        self.remember(&id, el);
        Ok(id)
    }

    fn cached(
        &self,
        el: &IUIAutomationElement,
        request: &IUIAutomationCacheRequest,
    ) -> Result<IUIAutomationElement, String> {
        let mut tries = 0;
        loop {
            // SAFETY: `el` and `request` are live UIA objects of this thread.
            match unsafe { el.BuildUpdatedCache(request) } {
                Ok(c) => return Ok(c),
                Err(e) if tries + 1 >= CACHE_TRIES => return Err(format!("UIA cache: {e}")),
                Err(_) => {
                    tries += 1;
                    std::thread::sleep(CACHE_RETRY);
                }
            }
        }
    }

    fn node_of(
        &mut self,
        el: &IUIAutomationElement,
        extras: Extras,
        chars: usize,
    ) -> Result<Node, String> {
        let r = raw(el);
        let kind = Kind {
            control_type: r.control_type,
            named: !r.name.is_empty(),
            toggles: r.toggles,
            selects: r.selected.is_some(),
            dialog: r.dialog || r.modal,
            submenu: r.expand.is_some_and(|e| e != LEAF),
        };
        let role = uia_roles::role(kind).unwrap_or("generic");
        let [x, y, w, h] = r.bounds;
        let value = r
            .value
            .clone()
            .filter(|v| !v.is_empty())
            .or_else(|| r.range.map(format_number))
            .map(|v| v.chars().take(chars).collect::<String>());
        Ok(Node {
            id: self.id_of(el)?,
            role,
            native_role: uia_roles::role(kind)
                .is_none()
                .then(|| uia_roles::native(r.control_type).to_owned()),
            name: r.name.trim_end().to_owned(),
            description: r.help.clone(),
            states: uia_roles::states(role, r.facts()),
            value: value.filter(|_| extras.values),
            shortcut: extras
                .keys
                .then(|| uia_combo(&r.accel, Os::Windows))
                .flatten(),
            mnemonic: extras
                .keys
                .then(|| uia_combo(&r.access, Os::Windows))
                .flatten(),
            bounds: (extras.bounds && !r.offscreen && w > 0 && h > 0 && is_active(role))
                .then_some([x, y, w, h]),
            ..Node::default()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        el: &IUIAutomationElement,
        parent: Option<usize>,
        depth: usize,
        extras: Extras,
        out: &mut Vec<Node>,
    ) -> Result<bool, String> {
        if out.len() >= MAX_NODES {
            return Ok(true);
        }
        let mut node = self.node_of(el, extras, VALUE_CHARS)?;
        if !is_field(node.role) {
            node.value = None;
        }
        let (parent, depth) = if node.role == "generic" && parent.is_some() {
            (parent, depth)
        } else {
            node.parent = parent;
            node.depth = depth;
            out.push(node);
            (Some(out.len() - 1), depth + 1)
        };
        if depth > MAX_DEPTH {
            return Ok(false);
        }
        for child in children(el) {
            if owned_dialog(&child) {
                continue;
            }
            if self.walk(&child, parent, depth, extras, out)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn find(
        &self,
        el: &IUIAutomationElement,
        scope: TreeScope,
        control_type: i32,
        request: &IUIAutomationCacheRequest,
    ) -> Vec<IUIAutomationElement> {
        let mut v = VARIANT::default();
        // SAFETY: the union field written matches the tag set with it.
        unsafe {
            (*v.Anonymous.Anonymous).vt = VT_I4;
            (*v.Anonymous.Anonymous).Anonymous.lVal = control_type;
        }
        // SAFETY: `self.auto`, `el` and `request` are live UIA objects of this thread; `v` is a valid VT_I4 VARIANT.
        let found = unsafe {
            self.auto
                .CreatePropertyCondition(UIA_ControlTypePropertyId, &v)
                .and_then(|c| el.FindAllBuildCache(scope, &c, request))
        };
        found.map(|a| list(&a)).unwrap_or_default()
    }

    fn frame_of(&mut self, el: &IUIAutomationElement, foreground: HWND) -> Option<Frame> {
        let r = raw(el);
        Some(Frame {
            id: self.id_of(el).ok()?,
            title: r.name.trim().to_owned(),
            role: if r.dialog || r.modal {
                "dialog"
            } else {
                "window"
            },
            pid: u32::try_from(r.pid).ok(),
            active: r.hwnd != 0 && r.hwnd == foreground.0 as isize,
        })
    }
}

impl Access for Uia {
    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        // SAFETY: GetForegroundWindow takes no arguments.
        let foreground = unsafe { GetForegroundWindow() };
        let root = self.root.clone();
        let single = self.single.clone();
        let mut frames = Vec::new();
        for top in self.find(&root, TreeScope_Children, WINDOW, &single) {
            for owned in self.find(&top, TreeScope_Children, WINDOW, &single) {
                frames.extend(self.frame_of(&owned, foreground));
            }
            frames.extend(self.frame_of(&top, foreground));
        }
        Ok(frames)
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let started = Instant::now();
        let el = self.element(&frame.id)?;
        let subtree = self.subtree.clone();
        let cached = self.cached(&el, &subtree)?;
        let fetched = started.elapsed().as_micros();
        let mut nodes = Vec::new();
        let mut truncated = self.walk(&cached, None, 0, extras, &mut nodes)?;
        let walked = started.elapsed().as_micros() - fetched;
        let root = self.root.clone();
        let mut menus = self.find(&root, TreeScope_Children, MENU, &subtree);
        menus.retain(|m| u32::try_from(raw(m).pid).ok() == frame.pid);
        if nodes.iter().any(|n| n.role == "menu" && n.has("expanded")) {
            menus.extend(self.find(&cached, TreeScope_Descendants, MENU, &subtree));
        }
        for menu in menus {
            let listed = runtime_id(&menu).is_ok_and(|id| nodes.iter().any(|n| n.id == id));
            if truncated || listed {
                continue;
            }
            truncated = self.walk(
                &menu,
                (!nodes.is_empty()).then_some(0),
                1,
                extras,
                &mut nodes,
            )?;
        }
        let menus = started.elapsed().as_micros() - fetched - walked;
        Ok(Tree {
            frame: frame.clone(),
            nodes,
            truncated,
            phases: vec![("items", fetched), ("walk", walked), ("menus", menus)],
        })
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let el = self.element(id)?;
        let single = self.single.clone();
        let fresh = self.cached(&el, &single)?;
        let mut node = self.node_of(&fresh, extras, chars)?;
        if extras.values && node.value.is_none() {
            node.value = document_text(&fresh, chars);
        }
        Ok(node)
    }

    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
        // SAFETY: `self.auto` is a live IUIAutomation of this thread.
        let walker =
            unsafe { self.auto.ControlViewWalker() }.map_err(|e| format!("UIA walker: {e}"))?;
        let mut chain = Vec::new();
        let mut at = self.element(id)?;
        while chain.len() < MAX_DEPTH {
            // SAFETY: `walker`, `at` and the root are live UIA objects of this thread.
            let parent = match unsafe { walker.GetParentElement(&at) } {
                Ok(p) => p,
                Err(_) => break,
            };
            // SAFETY: as above.
            if unsafe { self.auto.CompareElements(&parent, &self.root) }
                .is_ok_and(|same| same.as_bool())
            {
                break;
            }
            chain.push(self.id_of(&parent)?);
            at = parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
        // SAFETY: `self.auto` is a live IUIAutomation of this thread.
        match unsafe { self.auto.ElementFromPoint(POINT { x, y }) } {
            Ok(hit) => self.id_of(&hit).map(Some),
            Err(_) => Ok(None),
        }
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let el = self.element(id)?;
        // SAFETY: `el` is a live UIA element of this thread; each pattern comes from it.
        let done = unsafe {
            match act {
                Act::Focus => el.SetFocus().is_ok(),
                Act::Select => el
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                    .and_then(|p| p.Select())
                    .is_ok(),
                Act::Action => {
                    el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                        .and_then(|p| p.Invoke())
                        .is_ok()
                        || el
                            .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
                            .and_then(|p| p.Toggle())
                            .is_ok()
                        || el
                            .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                                UIA_ExpandCollapsePatternId,
                            )
                            .and_then(|p| {
                                if p.CurrentExpandCollapseState()? == ExpandCollapseState_Collapsed
                                {
                                    p.Expand()
                                } else {
                                    p.Collapse()
                                }
                            })
                            .is_ok()
                        || el
                            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                                UIA_SelectionItemPatternId,
                            )
                            .and_then(|p| p.Select())
                            .is_ok()
                        || el
                            .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                                UIA_LegacyIAccessiblePatternId,
                            )
                            .and_then(|p| p.DoDefaultAction())
                            .is_ok()
                }
            }
        };
        Ok(done)
    }

    fn close(&mut self, frame: &Frame) -> Result<(), String> {
        let el = self.element(&frame.id)?;
        // SAFETY: `el` is a live UIA element of this thread.
        let closed = unsafe {
            el.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
                .and_then(|p| p.Close())
        };
        if closed.is_ok() {
            return Ok(());
        }
        let hwnd = raw(&self.cached(&el, &self.single.clone())?).hwnd;
        // SAFETY: posting WM_CLOSE to a window handle has no memory effects; a stale handle fails.
        unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
            .map_err(|e| format!("close {:?}: {e}", frame.title))
    }
}

/// One element's cached properties; a property it lacks stays empty.
#[derive(Default)]
struct Raw {
    control_type: i32,
    name: String,
    help: String,
    enabled: bool,
    focused: bool,
    focusable: bool,
    offscreen: bool,
    bounds: [i32; 4],
    accel: String,
    access: String,
    pid: i32,
    hwnd: isize,
    toggle: Option<i32>,
    expand: Option<i32>,
    selected: Option<bool>,
    value: Option<String>,
    read_only: Option<bool>,
    range: Option<f64>,
    modal: bool,
    dialog: bool,
    toggles: bool,
}

impl Raw {
    fn facts(&self) -> Facts {
        Facts {
            enabled: self.enabled,
            focused: self.focused,
            focusable: self.focusable,
            offscreen: self.offscreen,
            modal: self.modal,
            read_only: self.read_only,
            toggle: self.toggle,
            expand: self.expand,
            selected: self.selected,
        }
    }
}

/// A cached property value.
enum Val {
    Int(i32),
    Bool(bool),
    Real(f64),
    Text(String),
    Other,
}

fn request(auto: &IUIAutomation, scope: TreeScope) -> Result<IUIAutomationCacheRequest, String> {
    // SAFETY: `auto` is a live IUIAutomation of this thread; the request is new and owned here.
    unsafe {
        let r = auto
            .CreateCacheRequest()
            .map_err(|e| format!("UIA cache request: {e}"))?;
        for p in PROPS {
            r.AddProperty(p)
                .map_err(|e| format!("UIA cache property {}: {e}", p.0))?;
        }
        r.SetTreeScope(scope)
            .map_err(|e| format!("UIA cache scope: {e}"))?;
        let view = auto
            .ControlViewCondition()
            .map_err(|e| format!("UIA control view: {e}"))?;
        r.SetTreeFilter(&view)
            .map_err(|e| format!("UIA cache filter: {e}"))?;
        Ok(r)
    }
}

fn raw(el: &IUIAutomationElement) -> Raw {
    let int = |p| match value(el, p) {
        Val::Int(i) => Some(i),
        _ => None,
    };
    let boolean = |p| match value(el, p) {
        Val::Bool(b) => Some(b),
        _ => None,
    };
    let text = |p| match value(el, p) {
        Val::Text(s) => s,
        _ => String::new(),
    };
    // SAFETY: `el` is a live UIA element of this thread whose cache holds these properties.
    let (rect, hwnd) = unsafe {
        (
            el.CachedBoundingRectangle().unwrap_or_default(),
            el.CachedNativeWindowHandle()
                .map(|h| h.0 as isize)
                .unwrap_or(0),
        )
    };
    let has = |p| boolean(p).unwrap_or(false);
    let toggles = has(UIA_IsTogglePatternAvailablePropertyId);
    let expands = has(UIA_IsExpandCollapsePatternAvailablePropertyId);
    let selects = has(UIA_IsSelectionItemPatternAvailablePropertyId);
    let values = has(UIA_IsValuePatternAvailablePropertyId);
    let ranges = has(UIA_IsRangeValuePatternAvailablePropertyId);
    let window = has(UIA_IsWindowPatternAvailablePropertyId);
    Raw {
        control_type: raw_control_type(el),
        name: text(UIA_NamePropertyId),
        help: text(UIA_HelpTextPropertyId),
        enabled: boolean(UIA_IsEnabledPropertyId).unwrap_or(true),
        focused: boolean(UIA_HasKeyboardFocusPropertyId).unwrap_or(false),
        focusable: boolean(UIA_IsKeyboardFocusablePropertyId).unwrap_or(false),
        offscreen: boolean(UIA_IsOffscreenPropertyId).unwrap_or(false),
        bounds: [
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ],
        accel: text(UIA_AcceleratorKeyPropertyId),
        access: text(UIA_AccessKeyPropertyId),
        pid: int(UIA_ProcessIdPropertyId).unwrap_or(0),
        hwnd,
        toggle: toggles
            .then(|| int(UIA_ToggleToggleStatePropertyId))
            .flatten(),
        expand: expands
            .then(|| int(UIA_ExpandCollapseExpandCollapseStatePropertyId))
            .flatten(),
        selected: selects
            .then(|| boolean(UIA_SelectionItemIsSelectedPropertyId))
            .flatten(),
        value: match values.then(|| value(el, UIA_ValueValuePropertyId)) {
            Some(Val::Text(s)) => Some(s),
            _ => None,
        },
        read_only: values
            .then(|| boolean(UIA_ValueIsReadOnlyPropertyId))
            .flatten(),
        range: match ranges.then(|| value(el, UIA_RangeValueValuePropertyId)) {
            Some(Val::Real(f)) => Some(f),
            _ => None,
        },
        modal: window && boolean(UIA_WindowIsModalPropertyId).unwrap_or(false),
        dialog: boolean(UIA_IsDialogPropertyId).unwrap_or(false),
        toggles,
    }
}

fn document_text(el: &IUIAutomationElement, chars: usize) -> Option<String> {
    let max = i32::try_from(chars).unwrap_or(i32::MAX);
    // SAFETY: `el` is a live UIA element of this thread; the text pattern and range come from it.
    let text = unsafe {
        el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .and_then(|p| p.DocumentRange())
            .and_then(|r| r.GetText(max))
    };
    text.ok().map(|t| t.to_string()).filter(|t| !t.is_empty())
}

fn owned_dialog(el: &IUIAutomationElement) -> bool {
    if raw_control_type(el) != WINDOW {
        return false;
    }
    let r = raw(el);
    r.dialog || r.modal
}

fn raw_control_type(el: &IUIAutomationElement) -> i32 {
    // SAFETY: `el` is a live UIA element of this thread whose cache holds its control type.
    unsafe { el.CachedControlType() }.map(|c| c.0).unwrap_or(0)
}

fn value(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Val {
    // SAFETY: `el` is a live UIA element of this thread; the VARIANT it returns is read by its tag and cleared once.
    unsafe {
        let Ok(mut v) = el.GetCachedPropertyValue(p) else {
            return Val::Other;
        };
        let inner = &v.Anonymous.Anonymous;
        let out = match inner.vt {
            VT_I4 => Val::Int(inner.Anonymous.lVal),
            VT_BOOL => Val::Bool(inner.Anonymous.boolVal.as_bool()),
            VT_R8 => Val::Real(inner.Anonymous.dblVal),
            VT_BSTR => Val::Text(inner.Anonymous.bstrVal.to_string()),
            _ => Val::Other,
        };
        let _ = VariantClear(&mut v);
        out
    }
}

fn children(el: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
    // SAFETY: `el` is a live UIA element of this thread built with a subtree cache.
    unsafe { el.GetCachedChildren() }
        .map(|a| list(&a))
        .unwrap_or_default()
}

fn list(a: &IUIAutomationElementArray) -> Vec<IUIAutomationElement> {
    // SAFETY: `a` is a live element array; indexes stay below its length.
    unsafe {
        let n = a.Length().unwrap_or(0);
        (0..n).filter_map(|i| a.GetElement(i).ok()).collect()
    }
}

fn runtime_id(el: &IUIAutomationElement) -> Result<String, String> {
    // SAFETY: `el` is a live UIA element; the SAFEARRAY it returns holds i32s and is destroyed once.
    unsafe {
        let ids: *mut SAFEARRAY = el
            .GetRuntimeId()
            .map_err(|e| format!("UIA runtime id: {e}"))?;
        if ids.is_null() {
            return Err("the element has no runtime id".to_owned());
        }
        let (lo, hi) = (
            SafeArrayGetLBound(ids, 1).unwrap_or(0),
            SafeArrayGetUBound(ids, 1).unwrap_or(-1),
        );
        let mut parts = Vec::new();
        for i in lo..=hi {
            let mut part = 0i32;
            if SafeArrayGetElement(ids, &i, (&raw mut part).cast()).is_ok() {
                parts.push(part.to_string());
            }
        }
        let _ = SafeArrayDestroy(ids);
        Ok(parts.join("-"))
    }
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        v.to_string()
    }
}
