package cb;

import android.app.UiAutomation;
import android.graphics.Rect;
import android.view.accessibility.AccessibilityNodeInfo;
import com.android.uiautomator.core.UiAutomationShellWrapper;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;

/** Serves the active window's accessibility tree, one dump per stdin line, in uiautomator's node format. */
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
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        PrintStream out = new PrintStream(System.out, false, "UTF-8");
        out.println("ready");
        out.flush();
        while (in.readLine() != null) {
            try {
                ui.waitForIdle(IDLE_MS, IDLE_TIMEOUT_MS);
            } catch (Exception e) {
                // a screen that never idles is dumped as it is
            }
            StringBuilder sb = new StringBuilder("<hierarchy>");
            AccessibilityNodeInfo root = ui.getRootInActiveWindow();
            if (root != null) {
                dump(root, sb, 0);
            }
            sb.append("</hierarchy>");
            out.println(sb);
            out.println(END);
            out.flush();
        }
        wrapper.disconnect();
    }

    private static void dump(AccessibilityNodeInfo n, StringBuilder sb, int depth) {
        if (depth > MAX_DEPTH) {
            return;
        }
        if (n.isVisibleToUser()) {
            Rect r = new Rect();
            n.getBoundsInScreen(r);
            sb.append("<node");
            attr(sb, "text", n.getText());
            attr(sb, "resource-id", n.getViewIdResourceName());
            attr(sb, "class", n.getClassName());
            attr(sb, "content-desc", n.getContentDescription());
            attr(sb, "checkable", String.valueOf(n.isCheckable()));
            attr(sb, "checked", String.valueOf(n.isChecked()));
            attr(sb, "clickable", String.valueOf(n.isClickable()));
            attr(sb, "long-clickable", String.valueOf(n.isLongClickable()));
            attr(sb, "editable", String.valueOf(n.isEditable()));
            attr(sb, "bounds", "[" + r.left + "," + r.top + "][" + r.right + "," + r.bottom + "]");
            sb.append(" />");
        }
        for (int i = 0; i < n.getChildCount(); i++) {
            AccessibilityNodeInfo c = n.getChild(i);
            if (c != null) {
                dump(c, sb, depth + 1);
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
}
