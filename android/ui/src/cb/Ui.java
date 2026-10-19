package cb;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.graphics.Rect;
import android.os.SystemClock;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;
import com.android.uiautomator.core.UiAutomationShellWrapper;
import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.lang.reflect.Method;
import java.util.List;

/**
 * Serves the accessibility tree from a resident app_process, one answer per stdin line, each ended by a line
 * "<<end>>": "windows" lists the interactive windows as JSON; "focus" the node with input focus as JSON (or null):
 * editable, password, whether its window has focus, and its text's length, hash and last 32 code points; "focus live"
 * the same, refreshed from the app rather than the accessibility cache; "paste" presses the paste key and answers once
 * the focused window has handled its down event (true) or the injection failed (false); "dump" the active window and
 * "dump ID" window ID as uiautomator-style nodes with their depth and child-index path.
 */
public final class Ui {
    private static final int MAX_DEPTH = 64;
    private static final long IDLE_MS = 100;
    private static final long IDLE_TIMEOUT_MS = 1000;
    private static final String END = "<<end>>";
    private static final int TAIL = 32;
    private static final int OUT_BUFFER = 1 << 20;
    private static final int INJECT_ASYNC = 0;
    private static final int INJECT_WAIT_FOR_FINISH = 2;

    private static Object inputManager;
    private static Method injectInputEvent;

    public static void main(String[] args) throws Exception {
        UiAutomationShellWrapper wrapper = new UiAutomationShellWrapper();
        wrapper.connect();
        wrapper.setCompressedLayoutHierarchy(false);
        UiAutomation ui = wrapper.getUiAutomation();
        AccessibilityServiceInfo info = ui.getServiceInfo();
        info.flags |= AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
        ui.setServiceInfo(info);
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        PrintStream out = new PrintStream(
                new BufferedOutputStream(new FileOutputStream(FileDescriptor.out), OUT_BUFFER), false, "UTF-8");
        out.println("ready");
        out.flush();
        String line;
        while ((line = in.readLine()) != null) {
            line = line.trim();
            try {
                if (line.equals("windows")) {
                    out.println(windows(ui));
                } else if (line.equals("focus") || line.equals("focus live")) {
                    out.println(focus(ui, line.equals("focus live")));
                } else if (line.equals("paste")) {
                    out.println(paste());
                } else {
                    idle(ui);
                    out.println(dump(ui, line.startsWith("dump ") ? line.substring(5).trim() : ""));
                }
            } catch (Exception e) {
                out.println("<error>" + e + "</error>");
            }
            out.println(END);
            out.flush();
        }
        wrapper.disconnect();
    }

    private static void idle(UiAutomation ui) {
        try {
            ui.waitForIdle(IDLE_MS, IDLE_TIMEOUT_MS);
        } catch (Exception e) {
            // a screen that never idles is dumped as it is
        }
    }

    private static String windows(UiAutomation ui) {
        StringBuilder sb = new StringBuilder("[");
        List<AccessibilityWindowInfo> list = ui.getWindows();
        for (int i = 0; i < list.size(); i++) {
            AccessibilityWindowInfo w = list.get(i);
            AccessibilityNodeInfo root = w.getRoot();
            Rect r = new Rect();
            w.getBoundsInScreen(r);
            if (i > 0) {
                sb.append(',');
            }
            sb.append("{\"id\":").append(w.getId())
                .append(",\"type\":").append(w.getType())
                .append(",\"layer\":").append(w.getLayer())
                .append(",\"title\":").append(json(w.getTitle()))
                .append(",\"package\":").append(json(root == null ? null : root.getPackageName()))
                .append(",\"active\":").append(w.isActive())
                .append(",\"focused\":").append(w.isFocused())
                .append(",\"bounds\":[").append(r.left).append(',').append(r.top).append(',')
                .append(r.right).append(',').append(r.bottom).append("]}");
        }
        return sb.append(']').toString();
    }

    private static String focus(UiAutomation ui, boolean live) {
        AccessibilityNodeInfo f = ui.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
        if (f == null || live && !f.refresh()) {
            return "null";
        }
        Rect r = new Rect();
        f.getBoundsInScreen(r);
        AccessibilityWindowInfo w = f.getWindow();
        CharSequence value = f.isShowingHintText() ? null : f.getText();
        String text = value == null ? "" : value.toString();
        int n = text.codePointCount(0, text.length());
        String tail = text.substring(text.offsetByCodePoints(0, Math.max(0, n - TAIL)));
        return "{\"editable\":" + f.isEditable() + ",\"password\":" + f.isPassword()
            + ",\"window\":" + (w != null && w.isFocused()) + ",\"len\":" + text.length()
            + ",\"hash\":" + text.hashCode() + ",\"tail\":" + json(tail) + ",\"class\":" + json(f.getClassName())
            + ",\"package\":" + json(f.getPackageName()) + ",\"bounds\":[" + r.left + ',' + r.top + ','
            + r.right + ',' + r.bottom + "]}";
    }

    private static boolean paste() throws ReflectiveOperationException {
        if (inputManager == null) {
            Class<?> c;
            try {
                c = Class.forName("android.hardware.input.InputManagerGlobal");
            } catch (ClassNotFoundException e) {
                c = Class.forName("android.hardware.input.InputManager");
            }
            injectInputEvent = c.getMethod("injectInputEvent", InputEvent.class, int.class);
            inputManager = c.getMethod("getInstance").invoke(null);
        }
        long now = SystemClock.uptimeMillis();
        return inject(pasteKey(now, KeyEvent.ACTION_DOWN), INJECT_WAIT_FOR_FINISH)
            && inject(pasteKey(now, KeyEvent.ACTION_UP), INJECT_ASYNC);
    }

    private static KeyEvent pasteKey(long now, int action) {
        return new KeyEvent(now, now, action, KeyEvent.KEYCODE_PASTE, 0, 0, KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0,
            InputDevice.SOURCE_KEYBOARD);
    }

    private static boolean inject(KeyEvent key, int mode) throws ReflectiveOperationException {
        return (Boolean) injectInputEvent.invoke(inputManager, key, mode);
    }

    private static String dump(UiAutomation ui, String id) {
        AccessibilityNodeInfo root = null;
        if (id.isEmpty()) {
            root = ui.getRootInActiveWindow();
        } else {
            for (AccessibilityWindowInfo w : ui.getWindows()) {
                if (String.valueOf(w.getId()).equals(id)) {
                    root = w.getRoot();
                }
            }
        }
        StringBuilder sb = new StringBuilder("<hierarchy>");
        if (root != null) {
            node(root, sb, 0, "0");
        }
        return sb.append("</hierarchy>").toString();
    }

    private static void node(AccessibilityNodeInfo n, StringBuilder sb, int depth, String path) {
        if (depth > MAX_DEPTH) {
            return;
        }
        Rect r = new Rect();
        n.getBoundsInScreen(r);
        sb.append("<node");
        attr(sb, "text", n.getText());
        attr(sb, "resource-id", n.getViewIdResourceName());
        attr(sb, "class", n.getClassName());
        attr(sb, "package", n.getPackageName());
        attr(sb, "content-desc", n.getContentDescription());
        attr(sb, "hint", n.getHintText());
        attr(sb, "showing-hint", String.valueOf(n.isShowingHintText()));
        attr(sb, "checkable", String.valueOf(n.isCheckable()));
        attr(sb, "checked", String.valueOf(n.isChecked()));
        attr(sb, "clickable", String.valueOf(n.isClickable()));
        attr(sb, "long-clickable", String.valueOf(n.isLongClickable()));
        attr(sb, "editable", String.valueOf(n.isEditable()));
        attr(sb, "enabled", String.valueOf(n.isEnabled()));
        attr(sb, "focused", String.valueOf(n.isFocused()));
        attr(sb, "selected", String.valueOf(n.isSelected()));
        attr(sb, "scrollable", String.valueOf(n.isScrollable()));
        attr(sb, "visible", String.valueOf(n.isVisibleToUser()));
        attr(sb, "depth", String.valueOf(depth));
        attr(sb, "path", path);
        attr(sb, "bounds", "[" + r.left + "," + r.top + "][" + r.right + "," + r.bottom + "]");
        sb.append(" />");
        for (int i = 0; i < n.getChildCount(); i++) {
            AccessibilityNodeInfo c = n.getChild(i);
            if (c != null) {
                node(c, sb, depth + 1, path + "." + i);
            }
        }
    }

    private static void attr(StringBuilder sb, String key, CharSequence value) {
        sb.append(' ').append(key).append("=\"");
        String v = value == null ? "" : value.toString();
        for (int i = 0; i < v.length(); i++) {
            char c = v.charAt(i);
            switch (c) {
                case '&': sb.append("&amp;"); break;
                case '<': sb.append("&lt;"); break;
                case '>': sb.append("&gt;"); break;
                case '"': sb.append("&quot;"); break;
                case '\n': sb.append("&#10;"); break;
                case '\r': sb.append("&#13;"); break;
                default: sb.append(c);
            }
        }
        sb.append('"');
    }

    private static String json(CharSequence value) {
        if (value == null) {
            return "\"\"";
        }
        StringBuilder sb = new StringBuilder("\"");
        String v = value.toString();
        for (int i = 0; i < v.length(); i++) {
            char c = v.charAt(i);
            if (c == '"' || c == '\\') {
                sb.append('\\').append(c);
            } else if (c < 0x20) {
                sb.append(String.format("\\u%04x", (int) c));
            } else {
                sb.append(c);
            }
        }
        return sb.append('"').toString();
    }
}
