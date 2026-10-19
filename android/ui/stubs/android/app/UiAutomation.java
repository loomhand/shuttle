package android.app;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.os.Looper;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;
import java.util.List;

public class UiAutomation {
    public static final int FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES = 1;
    public UiAutomation(Looper looper, IUiAutomationConnection connection) { throw new RuntimeException(); }
    public void connect(int flags) { throw new RuntimeException(); }
    public void disconnect() { throw new RuntimeException(); }
    public void waitForIdle(long idle, long timeout) throws java.util.concurrent.TimeoutException { throw new RuntimeException(); }
    public AccessibilityNodeInfo getRootInActiveWindow() { throw new RuntimeException(); }
    public List<AccessibilityWindowInfo> getWindows() { throw new RuntimeException(); }
    public AccessibilityServiceInfo getServiceInfo() { throw new RuntimeException(); }
    public void setServiceInfo(AccessibilityServiceInfo info) { throw new RuntimeException(); }
}
