//! The Windows desk backend: Win32 top-level windows and window trees from one cached UIA request; ids are runtime ids, or `uN` for an element whose runtime id is empty.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use desk_core::text::clip;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4, VT_R8, VariantClear};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GW_OWNER, GetAncestor, GetForegroundWindow, GetWindow, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, PostMessageW, SMTO_ABORTIFHUNG,
    SMTO_BLOCK, SendMessageTimeoutW, WM_CLOSE, WM_NULL, WindowFromPoint,
};
use windows::core::{BOOL, HRESULT, Interface, PWSTR};

use crate::keys::uia_combo;
use crate::lock;
use crate::model::{
    Access, Act, Extras, Frame, MAX_DEPTH, MAX_NODES, Node, Tree, VALUE_CHARS, evict_ids,
    format_number, is_active, lookup, place,
};
use crate::uia_roles::{self, Facts, Kind};

const ACT_TIMEOUT_MS: u32 = 2_000;
const TIMED_OUT: HRESULT = HRESULT(UIA_E_TIMEOUT.cast_signed());
const ACT_TIMED_OUT: &str = "the app did not answer the action within 2 s, as when the action opens a modal dialog; it may have taken effect: read the window before acting again";
const TITLE_CHARS: usize = 512;
const TAIL_LINES: i32 = 256;
const TAIL_ROUNDS: u32 = 12;
const ANSWER_MS: u32 = 500;
const PROPS: [UIA_PROPERTY_ID; 25] = [
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
    UIA_IsPasswordPropertyId,
];
const TOP_PROPS: [UIA_PROPERTY_ID; 4] = [
    UIA_ControlTypePropertyId,
    UIA_IsDialogPropertyId,
    UIA_WindowIsModalPropertyId,
    UIA_IsWindowPatternAvailablePropertyId,
];

static LAST: Mutex<BTreeMap<isize, Frame>> = Mutex::new(BTreeMap::new());
static MINTED: AtomicU64 = AtomicU64::new(0);

/// The UIA backend of this thread.
pub struct Uia {
    auto: IUIAutomation,
    subtree: IUIAutomationCacheRequest,
    children: IUIAutomationCacheRequest,
    single: IUIAutomationCacheRequest,
    top: IUIAutomationCacheRequest,
    root: IUIAutomationElement,
    ids: HashMap<String, IUIAutomationElement>,
    unnamed: HashMap<String, Vec<Walked>>,
    walking: Vec<Walked>,
    tops: HashMap<isize, Top>,
    walked: isize,
    shown: Vec<Shown>,
}

impl Uia {
    /// The backend on a COM multithreaded apartment, with per-monitor DPI awareness so bounds are pixels.
    pub fn open() -> Result<Self, String> {
        // SAFETY: `CUIAutomation8` is a static CLSID; a repeated COM or DPI setup returns S_FALSE or an ignored error.
        let auto: IUIAutomation = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
        }
        .map_err(|e| format!("start UI Automation: {e}"))?;
        let subtree = request(&auto, TreeScope_Subtree, &PROPS)?;
        let children = request(
            &auto,
            TreeScope(TreeScope_Element.0 | TreeScope_Children.0),
            &PROPS,
        )?;
        let single = request(&auto, TreeScope_Element, &PROPS)?;
        let top = request(&auto, TreeScope_Element, &TOP_PROPS)?;
        // SAFETY: `auto` is a live IUIAutomation on this thread.
        let root = unsafe { auto.GetRootElement() }.map_err(|e| format!("UIA root: {e}"))?;
        Ok(Self {
            auto,
            subtree,
            children,
            single,
            top,
            root,
            ids: HashMap::new(),
            unnamed: HashMap::new(),
            walking: Vec::new(),
            tops: HashMap::new(),
            walked: 0,
            shown: Vec::new(),
        })
    }

    fn element(&self, id: &str) -> Result<IUIAutomationElement, String> {
        lookup(&self.ids, id)
    }

    fn id_of(&mut self, el: &IUIAutomationElement) -> Result<String, String> {
        let runtime = runtime_id(el)?;
        let unnamed = runtime.is_empty();
        let id = own_id(runtime);
        if unnamed && let Some((_, _, list)) = self.walking.last_mut() {
            list.push((id.clone(), key(el)));
        }
        self.ids.entry(id.clone()).or_insert_with(|| el.clone());
        Ok(id)
    }

    fn found_id(
        &mut self,
        el: &IUIAutomationElement,
        point: Option<(i32, i32)>,
    ) -> Result<String, String> {
        let runtime = runtime_id(el)?;
        if !runtime.is_empty() {
            self.ids
                .entry(runtime.clone())
                .or_insert_with(|| el.clone());
            return Ok(runtime);
        }
        let seen = key(&self.cached(el, &self.single)?);
        let [x, y, w, h] = seen.2;
        let top = match point {
            Some((px, py)) => top_at(px, py),
            None if w > 0 && h > 0 => top_at(x + w / 2, y + h / 2),
            None => None,
        };
        let owners = top.map(owners_of).unwrap_or_default();
        let now = |id: &str| Some(key(&self.cached(self.ids.get(id)?, &self.single).ok()?));
        let walked = self.unnamed.values().flatten();
        match uia_roles::walked_id(walked, top, &owners, &seen, now) {
            Some(id) => Ok(id),
            None => self.id_of(el),
        }
    }

    fn native_root(&self, id: &str, chain: &[String]) -> Result<Option<isize>, String> {
        for held in std::iter::once(id).chain(chain.iter().rev().map(String::as_str)) {
            let el = self.element(held)?;
            // SAFETY: `el` is a live UIA element of this thread; a stale one answers an error.
            let hwnd = unsafe { el.CurrentNativeWindowHandle() }.unwrap_or_default();
            if !hwnd.is_invalid() {
                // SAFETY: GetAncestor takes any handle; a stale one answers a null handle.
                let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
                return Ok((!root.is_invalid()).then_some(root.0 as isize));
            }
        }
        Ok(None)
    }

    fn cached(
        &self,
        el: &IUIAutomationElement,
        request: &IUIAutomationCacheRequest,
    ) -> Result<IUIAutomationElement, String> {
        // SAFETY: `el` and `request` are live UIA objects of this thread.
        unsafe { el.BuildUpdatedCache(request) }.map_err(|e| format!("UIA cache: {e}"))
    }

    fn node_of(
        &mut self,
        el: &IUIAutomationElement,
        id: String,
        extras: Extras,
        chars: usize,
    ) -> Result<Node, String> {
        let mut r = raw(el, extras);
        let kind = Kind {
            control_type: r.control_type,
            named: !r.name.is_empty(),
            toggles: r.toggles,
            selects: r.facts.selected.is_some(),
            dialog: r.dialog || r.facts.modal,
            submenu: r
                .facts
                .expand
                .is_some_and(|e| e != ExpandCollapseState_LeafNode.0),
        };
        let mapped = uia_roles::role(kind);
        let role = mapped.unwrap_or("generic");
        let [x, y, w, h] = r.bounds;
        r.name.truncate(r.name.trim_end().len());
        let url = (extras.urls && role == "link")
            .then(|| r.value.take())
            .flatten()
            .filter(|u| !u.is_empty());
        let value = r
            .value
            .take()
            .filter(|v| !v.is_empty())
            .or_else(|| r.range.map(format_number))
            .map(|v| clip(v, chars, extras.tail));
        Ok(Node {
            id,
            role,
            native_role: mapped
                .is_none()
                .then(|| uia_roles::native(r.control_type).to_owned()),
            name: r.name,
            description: r.help,
            states: uia_roles::states(role, r.facts),
            value,
            shortcut: uia_combo(&r.accel),
            mnemonic: uia_combo(&r.access),
            bounds: (extras.bounds
                && !r.facts.offscreen
                && w > 0
                && h > 0
                && (extras.frame || is_active(role)))
            .then_some([x, y, w, h]),
            url,
            ..Node::default()
        })
    }

    fn walk(
        &mut self,
        el: &IUIAutomationElement,
        kids: &[IUIAutomationElement],
        parent: Option<usize>,
        depth: usize,
        extras: Extras,
        out: &mut Vec<Node>,
    ) -> Result<bool, String> {
        if out.len() >= MAX_NODES {
            return Ok(true);
        }
        let id = self.id_of(el)?;
        let node = self.node_of(el, id, extras, VALUE_CHARS)?;
        let (parent, depth) = place(out, node, parent, depth);
        if depth > MAX_DEPTH {
            return Ok(false);
        }
        for child in kids {
            if listed_dialog(child, self.walked, &self.shown) {
                continue;
            }
            if self.walk(child, &children(child), parent, depth, extras, out)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn top_of(&mut self, s: &Shown) -> Option<&Top> {
        let key = s.hwnd.0 as isize;
        if self.tops.get(&key).is_none_or(|t| t.pid != s.pid) {
            // SAFETY: `self.auto` and `self.top` are live UIA objects of this thread; a stale handle fails.
            let el = unsafe { self.auto.ElementFromHandleBuildCache(s.hwnd, &self.top) }.ok()?;
            let r = raw(&el, Extras::default());
            let top = Top {
                pid: s.pid,
                control_type: r.control_type,
                dialog: r.dialog || r.facts.modal,
                app: exe_name(s.pid),
                el,
            };
            self.tops.insert(key, top);
        }
        self.tops.get(&key)
    }

    fn roots(&mut self, shown: &[Shown]) -> Vec<usize> {
        let listed = |h: &isize| shown.iter().any(|s| s.hwnd.0 as isize == *h);
        self.tops.retain(|h, _| listed(h));
        let mut last = lock(&LAST);
        last.retain(|h, _| listed(h));
        uia_roles::forget_closed(&mut self.unnamed, &last);
        drop(last);
        (0..shown.len())
            .filter(|&i| {
                let owner = shown[i].owner;
                owner == 0 || !shown.iter().any(|o| o.hwnd.0 as isize == owner)
            })
            .collect()
    }

    fn owns_dialog(&mut self, hwnd: isize, shown: &[Shown]) -> bool {
        hwnd != 0
            && shown.iter().filter(|s| s.owner == hwnd).any(|s| {
                self.top_of(s)
                    .is_some_and(|t| t.control_type == UIA_WindowControlTypeId.0 && t.dialog)
            })
    }

    fn frame_of(
        &mut self,
        s: &Shown,
        foreground: HWND,
        answered: &mut HashMap<u32, bool>,
    ) -> Option<Frame> {
        let key = s.hwnd.0 as isize;
        let top = if s.hung || !answer_of(answered, s, || answers(s.hwnd)) {
            None
        } else {
            self.top_of(s)
        };
        if let Some(t) = top {
            if t.control_type != UIA_WindowControlTypeId.0 {
                return None;
            }
            let (dialog, app, el) = (t.dialog, t.app.clone(), t.el.clone());
            if let Ok(id) = self.id_of(&el) {
                let frame = Frame {
                    id,
                    title: title(s.hwnd).trim().to_owned(),
                    role: if dialog { "dialog" } else { "window" },
                    pid: Some(s.pid),
                    active: s.hwnd == foreground,
                    app,
                    number: u32::try_from(key).ok(),
                    ..Frame::default()
                };
                lock(&LAST).insert(key, frame.clone());
                return Some(frame);
            }
        }
        kept(&mut lock(&LAST), key, s.pid, s.hwnd == foreground)
    }
}

impl Access for Uia {
    fn foreground(&mut self) -> Option<i64> {
        Some(foreground().0 as i64)
    }

    fn in_view(&mut self, window: i64) -> Option<bool> {
        Some(visible(HWND(window as *mut core::ffi::c_void)))
    }

    fn focus_within(&mut self, window: i64) -> Option<bool> {
        // SAFETY: `self.auto` is a live IUIAutomation of this thread; a stale handle fails, and FindFirst finding nothing is an error.
        unsafe {
            let el = self
                .auto
                .ElementFromHandle(HWND(window as *mut core::ffi::c_void))
                .ok()?;
            let focused = self
                .auto
                .CreatePropertyCondition(UIA_HasKeyboardFocusPropertyId, &VARIANT::from(true))
                .ok()?;
            Some(el.FindFirst(TreeScope_Descendants, &focused).is_ok())
        }
    }

    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        let front = foreground();
        let shown = shown();
        let roots = self.roots(&shown);
        let (mut frames, mut answered) = (Vec::new(), HashMap::new());
        for i in roots {
            let Some(top) = self.frame_of(&shown[i], front, &mut answered) else {
                continue;
            };
            let found = owned(&shown, i)
                .into_iter()
                .filter_map(|o| Some((o, self.frame_of(&shown[o], front, &mut answered)?)))
                .collect();
            frames.extend(link(&shown, i, &top, found));
            frames.push(top);
        }
        stale_front(&mut frames, &mut lock(&LAST), front.0 as isize);
        Ok(frames)
    }

    fn evict(&mut self) {
        if evict_ids(&mut self.ids, lock(&LAST).values()) {
            self.unnamed.clear();
        }
    }

    fn hint(&mut self) -> Option<u64> {
        let (mut h, mut buf) = (DefaultHasher::new(), [0u16; TITLE_CHARS]);
        (foreground().0 as isize).hash(&mut h);
        for s in shown() {
            (
                s.hwnd.0 as isize,
                s.owner,
                s.pid,
                title_units(s.hwnd, &mut buf),
            )
                .hash(&mut h);
        }
        Some(h.finish())
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let started = Instant::now();
        let el = self.element(&frame.id)?;
        // a subtree cache of a window spans the dialogs it owns, which the walk skips
        let shown = shown();
        // SAFETY: `el` is a live UIA element of this thread.
        let hwnd = unsafe { el.CurrentNativeWindowHandle() }.map_or(0, |h| h.0 as isize);
        let owns = self.owns_dialog(hwnd, &shown);
        (self.walked, self.shown) = (hwnd, shown);
        let cached = self.cached(&el, if owns { &self.children } else { &self.subtree })?;
        let mut kids = children(&cached);
        if owns {
            kids = kids
                .iter()
                .filter(|c| !listed_dialog(c, self.walked, &self.shown))
                .filter_map(|c| self.cached(c, &self.subtree).ok())
                .collect();
        }
        let fetched = started.elapsed().as_micros();
        let mut nodes = Vec::new();
        self.walking = vec![(self.walked, false, Vec::new())];
        let mut truncated = self.walk(&cached, &kids, None, 0, extras, &mut nodes)?;
        let walked = started.elapsed().as_micros() - fetched;
        if !truncated {
            let shown = self.shown.clone();
            let roots = self.roots(&shown);
            let menus: Vec<_> = roots
                .into_iter()
                .filter(|&i| Some(shown[i].pid) == frame.pid)
                .filter_map(|i| {
                    let t = self.top_of(&shown[i])?;
                    let el = (t.control_type == UIA_MenuControlTypeId.0).then(|| t.el.clone())?;
                    Some((
                        shown[i].hwnd.0 as isize,
                        self.cached(&el, &self.subtree).ok()?,
                    ))
                })
                .collect();
            for (popup, menu) in menus {
                let listed = runtime_id(&menu)
                    .ok()
                    .and_then(|id| nodes.iter().position(|n| n.id == id));
                if let Some(root) = listed
                    && let Some((_, _, frame)) = self.walking.first_mut()
                {
                    let entries = uia_roles::popup_entries(&nodes, root, frame);
                    self.walking.push((popup, false, entries));
                }
                if truncated || listed.is_some() {
                    continue;
                }
                self.walking.push((popup, false, Vec::new()));
                truncated = self.walk(
                    &menu,
                    &children(&menu),
                    (!nodes.is_empty()).then_some(0),
                    1,
                    extras,
                    &mut nodes,
                )?;
            }
        }
        let menus = started.elapsed().as_micros() - fetched - walked;
        if let Some((_, _, frame)) = self.walking.first_mut() {
            let owned = uia_roles::owned_entries(&nodes, frame);
            self.walking.push((self.walked, true, owned));
        }
        let unnamed = std::mem::take(&mut self.walking);
        self.unnamed.insert(frame.id.clone(), unnamed);
        Ok(Tree {
            frame: frame.clone(),
            nodes,
            truncated,
            phases: vec![("items", fetched), ("walk", walked), ("menus", menus)],
        })
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let el = self.element(id)?;
        let fresh = self.cached(&el, &self.single)?;
        let mut node = self.node_of(&fresh, id.to_owned(), extras, chars)?;
        if extras.values
            && node.value.is_none()
            && node.url.is_none()
            && let Some((text, cut)) = document_text(&fresh, chars, extras.tail)
        {
            node.value = Some(text);
            node.truncated = cut;
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
            // SAFETY: `walker` and `at` are live UIA objects of this thread.
            let Ok(parent) = (unsafe { walker.GetParentElement(&at) }) else {
                break;
            };
            if same(&self.auto, &parent, &self.root) {
                break;
            }
            chain.push(self.found_id(&parent, None)?);
            at = parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
        Ok(process_id(&self.element(id)?))
    }

    fn menu_cover(
        &mut self,
        id: &str,
        chain: &[String],
        x: i32,
        y: i32,
    ) -> Result<Option<(String, Option<u32>)>, String> {
        let own = self
            .native_root(id, chain)?
            .ok_or("the menu entry's own window could not be read")?;
        let at = top_at(x, y);
        if uia_roles::menu_click_reaches(at, Some(own), owners_of) {
            return Ok(None);
        }
        Ok(Some(at.map_or((String::new(), None), |w| {
            let hwnd = HWND(w as *mut core::ffi::c_void);
            let mut pid = 0u32;
            // SAFETY: GetWindowThreadProcessId takes any handle and writes the pid into a live u32; a stale handle writes nothing.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
            (title(hwnd).trim().to_owned(), (pid != 0).then_some(pid))
        })))
    }

    fn pid_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
        let _bounded = Bounded::new(&self.auto);
        let hit = element_at(&self.auto, x, y)?;
        process_id(&hit)
            .map(Some)
            .ok_or_else(|| format!("the process of the element at ({x}, {y}) could not be read"))
    }

    fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
        let _bounded = Bounded::new(&self.auto);
        let hit = element_at(&self.auto, x, y)?;
        self.found_id(&hit, Some((x, y))).map(Some)
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let el = self.element(id)?;
        let _bounded = Bounded::new(&self.auto);
        // SAFETY: `el` is a live UIA element of this thread; each pattern comes from it.
        let done = unsafe {
            let select = || {
                tried(
                    el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                    .and_then(|p| p.Select()),
                )
            };
            match act {
                Act::Focus => tried(el.SetFocus()),
                Act::Select => select(),
                Act::Action => tried(
                    el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                        .and_then(|p| p.Invoke()),
                )
                .or_else(|| {
                    tried(
                        el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
                            .and_then(|p| p.Toggle()),
                    )
                })
                .or_else(|| {
                    tried(
                        el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                            UIA_ExpandCollapsePatternId,
                        )
                        .and_then(|p| {
                            if p.CurrentExpandCollapseState()? == ExpandCollapseState_Collapsed {
                                p.Expand()
                            } else {
                                p.Collapse()
                            }
                        }),
                    )
                })
                .or_else(select)
                .or_else(|| {
                    tried(
                        el.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                            UIA_LegacyIAccessiblePatternId,
                        )
                        .and_then(|p| p.DoDefaultAction()),
                    )
                }),
            }
        };
        done.unwrap_or(Ok(false))
    }

    fn close(&mut self, frame: &Frame) -> Result<(), String> {
        let el = self.element(&frame.id)?;
        let bounded = Bounded::new(&self.auto);
        // SAFETY: `el` is a live UIA element of this thread.
        let closed = unsafe {
            el.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
                .and_then(|p| p.Close())
        };
        drop(bounded);
        if let Some(closed) = tried(closed) {
            return closed.map(drop);
        }
        // SAFETY: `el` is a live UIA element; posting WM_CLOSE to a window handle has no memory effects.
        unsafe {
            let hwnd = el
                .CurrentNativeWindowHandle()
                .map_err(|e| format!("close {:?}: {e}", frame.title))?;
            PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0))
        }
        .map_err(|e| format!("close {:?}: {e}", frame.title))
    }
}

struct Bounded(Option<(IUIAutomation2, u32)>);

impl Bounded {
    fn new(auto: &IUIAutomation) -> Self {
        let Ok(auto) = auto.cast::<IUIAutomation2>() else {
            return Self(None);
        };
        // SAFETY: `auto` is a live IUIAutomation2 of this thread; the timeout is a client-side property.
        let was = unsafe {
            auto.TransactionTimeout()
                .and_then(|was| auto.SetTransactionTimeout(ACT_TIMEOUT_MS).map(|()| was))
        };
        Self(was.ok().map(|was| (auto, was)))
    }
}

impl Drop for Bounded {
    fn drop(&mut self) {
        if let Some((auto, was)) = &self.0 {
            // SAFETY: `auto` is a live IUIAutomation2 of this thread; the timeout is a client-side property.
            let _ = unsafe { auto.SetTransactionTimeout(*was) };
        }
    }
}

struct Top {
    pid: u32,
    control_type: i32,
    dialog: bool,
    app: Option<String>,
    el: IUIAutomationElement,
}

#[derive(Clone)]
struct Shown {
    hwnd: HWND,
    owner: isize,
    pid: u32,
    thread: u32,
    hung: bool,
}

/// One element's cached properties; a property it lacks stays empty.
struct Raw {
    control_type: i32,
    name: String,
    help: String,
    bounds: [i32; 4],
    accel: String,
    access: String,
    value: Option<String>,
    range: Option<f64>,
    dialog: bool,
    toggles: bool,
    facts: Facts,
}

enum Val {
    Int(i32),
    Bool(bool),
    Real(f64),
    Text(String),
    Other,
}

type Key = (i32, String, [i32; 4]);
type Walked = (isize, bool, Vec<(String, Key)>);

/// Whether the windows `a` and `b` belong to one process.
pub(crate) fn same_process(a: i64, b: i64) -> bool {
    let pid = |w: i64| {
        let mut pid = 0u32;
        // SAFETY: GetWindowThreadProcessId takes any handle and writes the pid into a live u32; a stale handle writes nothing.
        unsafe { GetWindowThreadProcessId(HWND(w as *mut core::ffi::c_void), Some(&raw mut pid)) };
        pid
    };
    let (pa, pb) = (pid(a), pid(b));
    pa != 0 && pa == pb
}

fn tried(r: windows::core::Result<()>) -> Option<Result<bool, String>> {
    match r {
        Ok(()) => Some(Ok(true)),
        Err(e) if e.code() == TIMED_OUT => Some(Err(ACT_TIMED_OUT.to_owned())),
        Err(_) => None,
    }
}

fn request(
    auto: &IUIAutomation,
    scope: TreeScope,
    props: &[UIA_PROPERTY_ID],
) -> Result<IUIAutomationCacheRequest, String> {
    // SAFETY: `auto` is a live IUIAutomation of this thread; the request is new and owned here.
    unsafe {
        let r = auto
            .CreateCacheRequest()
            .map_err(|e| format!("UIA cache request: {e}"))?;
        for &p in props {
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

fn raw(el: &IUIAutomationElement, extras: Extras) -> Raw {
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
    // SAFETY: `el` is a live UIA element of this thread; a property its cache lacks is an error read as the default.
    let rect = unsafe { el.CachedBoundingRectangle() }.unwrap_or_default();
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
        bounds: [
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ],
        accel: if extras.keys {
            text(UIA_AcceleratorKeyPropertyId)
        } else {
            String::new()
        },
        access: if extras.keys {
            text(UIA_AccessKeyPropertyId)
        } else {
            String::new()
        },
        value: (values && (extras.values || extras.urls)).then(|| text(UIA_ValueValuePropertyId)),
        range: match (ranges && extras.values).then(|| value(el, UIA_RangeValueValuePropertyId)) {
            Some(Val::Real(f)) => Some(f),
            _ => None,
        },
        dialog: has(UIA_IsDialogPropertyId),
        toggles,
        facts: Facts {
            enabled: boolean(UIA_IsEnabledPropertyId).unwrap_or(true),
            focused: has(UIA_HasKeyboardFocusPropertyId),
            focusable: has(UIA_IsKeyboardFocusablePropertyId),
            offscreen: has(UIA_IsOffscreenPropertyId),
            password: has(UIA_IsPasswordPropertyId),
            modal: window && has(UIA_WindowIsModalPropertyId),
            read_only: values
                .then(|| boolean(UIA_ValueIsReadOnlyPropertyId))
                .flatten(),
            toggle: toggles
                .then(|| int(UIA_ToggleToggleStatePropertyId))
                .flatten(),
            expand: expands
                .then(|| int(UIA_ExpandCollapseExpandCollapseStatePropertyId))
                .flatten(),
            selected: selects
                .then(|| boolean(UIA_SelectionItemIsSelectedPropertyId))
                .flatten(),
        },
    }
}

fn shown() -> Vec<Shown> {
    unsafe extern "system" fn add(hwnd: HWND, list: LPARAM) -> BOOL {
        // SAFETY: `list` is the `Vec<HWND>` that `shown` lends for the duration of EnumWindows.
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes to `all`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(add), LPARAM(&raw mut all as isize)) };
    all.into_iter()
        .filter(|&hwnd| visible(hwnd))
        .map(|hwnd| {
            let mut pid = 0u32;
            // SAFETY: the window calls take a handle; `pid` is a live u32 they write.
            let (thread, owner, hung) = unsafe {
                (
                    GetWindowThreadProcessId(hwnd, Some(&raw mut pid)),
                    GetWindow(hwnd, GW_OWNER).map_or(0, |o| o.0 as isize),
                    IsHungAppWindow(hwnd).as_bool(),
                )
            };
            Shown {
                hwnd,
                owner,
                pid,
                thread,
                hung,
            }
        })
        .collect()
}

fn answers(hwnd: HWND) -> bool {
    // SAFETY: WM_NULL carries no pointers; the call waits at most ANSWER_MS for the window's thread.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            ANSWER_MS,
            None,
        )
    }
    .0 != 0
}

fn answer_of(answered: &mut HashMap<u32, bool>, s: &Shown, probe: impl FnOnce() -> bool) -> bool {
    *answered.entry(s.thread).or_insert_with(probe)
}

fn kept(last: &mut BTreeMap<isize, Frame>, hwnd: isize, pid: u32, front: bool) -> Option<Frame> {
    if last.get(&hwnd).is_some_and(|f| f.pid != Some(pid)) {
        last.remove(&hwnd);
    }
    let f = last.get_mut(&hwnd)?;
    f.active |= front;
    Some(Frame {
        stale: true,
        ..f.clone()
    })
}

fn stale_front(frames: &mut [Frame], last: &mut BTreeMap<isize, Frame>, front: isize) {
    let listed = last
        .get(&front)
        .map(|f| f.id.clone())
        .filter(|id| frames.iter().any(|f| &f.id == id));
    if let Some(id) = listed {
        for f in frames.iter_mut() {
            f.active = f.id == id;
        }
        for (_, f) in last.iter_mut().filter(|(h, _)| **h != front) {
            f.active = false;
        }
    }
}

fn owned(shown: &[Shown], root: usize) -> Vec<usize> {
    let mut owners = vec![shown[root].hwnd.0 as isize];
    let mut found = Vec::new();
    while let Some(owner) = owners.pop() {
        for i in (0..shown.len()).filter(|&i| shown[i].owner == owner) {
            owners.push(shown[i].hwnd.0 as isize);
            found.push(i);
        }
    }
    found.reverse();
    found
}

fn link(shown: &[Shown], root: usize, top: &Frame, found: Vec<(usize, Frame)>) -> Vec<Frame> {
    let mut ids: HashMap<isize, String> = found
        .iter()
        .map(|(o, f)| (shown[*o].hwnd.0 as isize, f.id.clone()))
        .collect();
    ids.insert(shown[root].hwnd.0 as isize, top.id.clone());
    found
        .into_iter()
        .map(|(o, mut f)| {
            f.owner = ids.get(&shown[o].owner).cloned();
            f
        })
        .collect()
}

fn title(hwnd: HWND) -> String {
    String::from_utf16_lossy(title_units(hwnd, &mut [0u16; TITLE_CHARS]))
}

fn title_units(hwnd: HWND, buf: &mut [u16; TITLE_CHARS]) -> &[u16] {
    // SAFETY: GetWindowTextW writes at most `buf.len()` UTF-16 units into `buf`.
    let n = unsafe { GetWindowTextW(hwnd, buf) };
    &buf[..usize::try_from(n).unwrap_or(0)]
}

fn visible(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the window calls take a handle and write at most the 4 bytes of `cloaked`.
    unsafe {
        IsWindowVisible(hwnd).as_bool()
            && (DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&raw mut cloaked).cast(),
                size_of::<u32>() as u32,
            )
            .is_err()
                || cloaked == 0)
    }
}

fn foreground() -> HWND {
    // SAFETY: GetForegroundWindow takes no arguments.
    unsafe { GetForegroundWindow() }
}

fn element_at(auto: &IUIAutomation, x: i32, y: i32) -> Result<IUIAutomationElement, String> {
    // SAFETY: `auto` is a live IUIAutomation of this thread.
    unsafe { auto.ElementFromPoint(POINT { x, y }) }
        .map_err(|e| format!("the UI Automation hit test at ({x}, {y}) failed: {e}"))
}

fn process_id(el: &IUIAutomationElement) -> Option<u32> {
    // SAFETY: `el` is a live UIA element of this thread.
    unsafe { el.CurrentProcessId() }
        .ok()
        .and_then(|p| u32::try_from(p).ok())
}

fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess takes a pid; the handle it returns is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `process` is a live handle and `buf` holds `len` UTF-16 units.
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` is a handle this function opened.
    let _ = unsafe { CloseHandle(process) };
    named.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().map(str::to_owned)
}

fn document_text(el: &IUIAutomationElement, chars: usize, tail: bool) -> Option<(String, bool)> {
    // SAFETY: `el` is a live UIA element of this thread; the text pattern and range come from it.
    let read = unsafe {
        el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .and_then(|p| p.DocumentRange())
            .and_then(|doc| {
                if !tail {
                    let text = doc.GetText(head_units(chars))?.to_string();
                    let cut = text.chars().nth(chars).is_some();
                    return Ok((clip(text, chars, false), cut));
                }
                let r = doc.Clone()?;
                r.MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &doc,
                    TextPatternRangeEndpoint_End,
                )?;
                let mut round = 0;
                loop {
                    let step = TAIL_LINES << round;
                    let moved =
                        r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Line, -step)?;
                    let text = r.GetText(-1)?.to_string();
                    let at_start = moved > -step
                        || r.CompareEndpoints(
                            TextPatternRangeEndpoint_Start,
                            &doc,
                            TextPatternRangeEndpoint_Start,
                        )? <= 0;
                    if let Some(cut) = tail_stop(text.chars().count(), chars, at_start, round) {
                        return Ok((clip(text, chars, true), cut));
                    }
                    round += 1;
                }
            })
    };
    read.ok().filter(|(t, _)| !t.is_empty())
}

fn head_units(chars: usize) -> i32 {
    i32::try_from(chars.saturating_mul(2).saturating_add(1)).unwrap_or(i32::MAX)
}

fn tail_stop(scalars: usize, chars: usize, at_start: bool, round: u32) -> Option<bool> {
    if scalars > chars {
        Some(true)
    } else if at_start {
        Some(false)
    } else {
        (round + 1 >= TAIL_ROUNDS).then_some(true)
    }
}

fn listed_dialog(el: &IUIAutomationElement, walked: isize, shown: &[Shown]) -> bool {
    if raw_control_type(el) != UIA_WindowControlTypeId.0 {
        return false;
    }
    let flag = |p| matches!(value(el, p), Val::Bool(true));
    let dialog = flag(UIA_IsDialogPropertyId)
        || flag(UIA_IsWindowPatternAvailablePropertyId) && flag(UIA_WindowIsModalPropertyId);
    // SAFETY: `el` is a live UIA element of this thread.
    dialog
        && unsafe { el.CurrentNativeWindowHandle() }
            .is_ok_and(|h| own_frame(h.0 as isize, walked, shown))
}

fn own_frame(hwnd: isize, walked: isize, shown: &[Shown]) -> bool {
    hwnd != 0 && hwnd != walked && shown.iter().any(|s| s.hwnd.0 as isize == hwnd)
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
        let mut id = String::new();
        for i in lo..=hi {
            let mut part = 0i32;
            if SafeArrayGetElement(ids, &i, (&raw mut part).cast()).is_ok() {
                if !id.is_empty() {
                    id.push('-');
                }
                let _ = write!(id, "{part}");
            }
        }
        let _ = SafeArrayDestroy(ids);
        Ok(id)
    }
}

fn own_id(runtime: String) -> String {
    if !runtime.is_empty() {
        return runtime;
    }
    format!("u{}", MINTED.fetch_add(1, Ordering::Relaxed) + 1)
}

fn key(el: &IUIAutomationElement) -> Key {
    let r = raw(el, Extras::default());
    (r.control_type, r.name, r.bounds)
}

fn top_at(x: i32, y: i32) -> Option<isize> {
    // SAFETY: WindowFromPoint takes a point and GetAncestor a handle; neither writes through a pointer.
    let top = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
    (!top.is_invalid()).then_some(top.0 as isize)
}

fn owners_of(top: isize) -> Vec<isize> {
    let mut owners = Vec::new();
    let mut at = HWND(top as *mut core::ffi::c_void);
    while owners.len() < MAX_DEPTH {
        // SAFETY: GetWindow takes any handle; a stale or unowned one answers an error or a null handle.
        let Ok(owner) = (unsafe { GetWindow(at, GW_OWNER) }) else {
            break;
        };
        if owner.is_invalid() {
            break;
        }
        owners.push(owner.0 as isize);
        at = owner;
    }
    owners
}

fn same(auto: &IUIAutomation, a: &IUIAutomationElement, b: &IUIAutomationElement) -> bool {
    // SAFETY: `auto`, `a` and `b` are live UIA objects of this thread.
    unsafe { auto.CompareElements(a, b) }.is_ok_and(BOOL::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_can_bound_an_action_by_a_transaction_timeout() {
        let uia = Uia::open().expect("UI Automation starts");
        assert!(
            Bounded::new(&uia.auto).0.is_some(),
            "the automation object lacks IUIAutomation2"
        );
    }

    #[test]
    fn a_tail_text_read_stops_once_it_holds_more_than_its_limit_or_reaches_the_start() {
        assert_eq!(tail_stop(65_538, 65_537, false, 0), Some(true));
        assert_eq!(tail_stop(65_538, 65_537, true, 3), Some(true));
        assert_eq!(tail_stop(65_537, 65_537, true, 0), Some(false));
        assert_eq!(tail_stop(10, 65_537, true, 5), Some(false));
        assert_eq!(tail_stop(10, 65_537, false, 0), None);
        assert_eq!(tail_stop(10, 65_537, false, TAIL_ROUNDS - 2), None);
        assert_eq!(tail_stop(10, 65_537, false, TAIL_ROUNDS - 1), Some(true));
    }

    #[test]
    fn a_head_text_read_asks_for_two_units_a_character_and_one_more() {
        assert_eq!(head_units(65_537), 131_075);
        assert_eq!(head_units(80), 161);
        assert_eq!(head_units(usize::MAX), i32::MAX);
    }

    #[test]
    fn an_element_with_an_empty_runtime_id_gets_an_id_no_other_element_shares_across_backends() {
        assert_eq!(own_id("42-656186".into()), "42-656186");
        let walk = || (0..10).map(|_| own_id(String::new())).collect::<Vec<_>>();
        let (first, reopened) = (walk(), walk());
        let all: std::collections::HashSet<_> = first.iter().chain(&reopened).collect();
        assert_eq!(all.len(), 20);
        assert!(all.iter().all(|id| id.starts_with('u') && id.len() > 1));
    }

    #[test]
    fn only_a_dialog_with_a_listed_window_of_its_own_leaves_the_tree() {
        let shown = [window(7, 0), window(9, 7)];
        assert!(own_frame(9, 7, &shown));
        assert!(!own_frame(0, 7, &shown));
        assert!(!own_frame(7, 7, &shown));
        assert!(!own_frame(8, 7, &shown));
    }

    #[test]
    fn a_dialog_owned_by_a_dialog_comes_with_its_root_window_innermost_first() {
        let shown = [window(3, 2), window(2, 1), window(1, 0), window(4, 0)];
        assert_eq!(owned(&shown, 2), vec![0, 1]);
        assert!(owned(&shown, 3).is_empty());
    }

    #[test]
    fn a_nested_owned_alert_names_the_dialog_that_owns_it() {
        let shown = [window(3, 2), window(2, 1), window(1, 0)];
        let frame = |id: &str| Frame {
            id: id.into(),
            ..Frame::default()
        };
        let found = owned(&shown, 2)
            .into_iter()
            .map(|o| (o, frame(["alert", "dialog"][o])))
            .collect();
        let owners: Vec<_> = link(&shown, 2, &frame("root"), found)
            .into_iter()
            .map(|f| (f.id, f.owner))
            .collect();
        assert_eq!(
            owners,
            [
                ("alert".to_owned(), Some("dialog".to_owned())),
                ("dialog".to_owned(), Some("root".to_owned()))
            ]
        );
    }

    #[test]
    fn a_window_that_stopped_answering_keeps_its_last_frame_stale_and_active() {
        let frame = |id: &str, active: bool| Frame {
            id: id.into(),
            title: id.into(),
            pid: Some(1),
            active,
            ..Frame::default()
        };
        let mut last = BTreeMap::from([(7, frame("hung", true)), (8, frame("left", false))]);
        assert!(kept(&mut last, 9, 1, false).is_none());
        let mut frames = vec![frame("other", false), kept(&mut last, 7, 1, false).unwrap()];
        stale_front(&mut frames, &mut last, 0);
        assert!(frames[1].stale && frames[1].active && frames[1].title == "hung");
    }

    #[test]
    fn a_listed_front_window_is_the_only_active_one_and_clears_the_others_last_state() {
        let frame = |id: &str, active: bool| Frame {
            id: id.into(),
            pid: Some(1),
            active,
            ..Frame::default()
        };
        let mut last = BTreeMap::from([(1, frame("allowed", true)), (2, frame("other", false))]);
        let mut frames = vec![
            kept(&mut last, 2, 1, true).unwrap(),
            kept(&mut last, 1, 1, false).unwrap(),
        ];
        assert!(frames.iter().all(|f| f.active));
        stale_front(&mut frames, &mut last, 2);
        assert!(frames[0].active && !frames[1].active);
        assert!(last[&2].active && !last[&1].active);
    }

    #[test]
    fn a_stale_window_that_was_in_front_stays_active_once_its_ghost_takes_the_foreground() {
        let left = Frame {
            pid: Some(1),
            ..Frame::default()
        };
        let mut last = BTreeMap::from([(8, left)]);
        assert!(kept(&mut last, 8, 1, true).unwrap().active);
        assert!(kept(&mut last, 8, 1, false).unwrap().active);
    }

    #[test]
    fn a_recycled_window_handle_of_another_process_has_no_last_frame() {
        let old = Frame {
            pid: Some(1),
            active: true,
            ..Frame::default()
        };
        let mut last = BTreeMap::from([(8, old)]);
        assert!(kept(&mut last, 8, 2, true).is_none());
        assert!(last.is_empty());
    }

    #[test]
    fn one_probe_answers_for_every_window_of_its_thread_in_a_listing() {
        let (mut answered, mut probes) = (HashMap::new(), 0);
        let thread = |t| Shown {
            thread: t,
            ..window(1, 0)
        };
        let mut probe = |t, ok| {
            answer_of(&mut answered, &thread(t), || {
                probes += 1;
                ok
            })
        };
        assert!(!probe(10, false));
        assert!(!probe(10, true));
        assert!(probe(11, true));
        assert!(probe(11, false));
        assert_eq!(probes, 2);
    }

    fn window(hwnd: isize, owner: isize) -> Shown {
        Shown {
            hwnd: HWND(hwnd as *mut std::ffi::c_void),
            owner,
            pid: 1,
            thread: 1,
            hung: false,
        }
    }
}
