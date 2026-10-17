package cb;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.graphics.Rect;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;
import com.android.uiautomator.core.UiAutomationShellWrapper;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.util.List;

/**
 * Serves the accessibility tree from a resident app_process, one answer per stdin line, each ended by a line
 * "<<end>>": "windows" lists the interactive windows as JSON, "dump" the active window and "dump ID" window ID
 * as uiautomator-style nodes with their depth and child-index path.
 */
public final class Ui {
    private static final int MAX_DEPTH = 64;
    private static final long IDLE_MS = 100;
    private static final long IDLE_TIMEOUT_MS = 1000;
    private static final String END = "<<end>>";

    public static void main(String[] args) throws Exception {
        UiAutomationShellWrapper wrapper = new UiAutomationShellWrapper();
        wrapper.connect();
        wrapper.setCompressedLayoutHierarchy(false);
        UiAutomation ui = wrapper.getUiAutomation();
        AccessibilityServiceInfo info = ui.getServiceInfo();
        info.flags |= AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
        ui.setServiceInfo(info);
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        PrintStream out = new PrintStream(System.out, false, "UTF-8");
        out.println("ready");
        out.flush();
        String line;
        while ((line = in.readLine()) != null) {
            line = line.trim();
            try {
                if (line.equals("windows")) {
                    out.println(windows(ui));
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
