package android.app;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;
import java.util.List;

public class UiAutomation {
    public void waitForIdle(long idle, long timeout) throws java.util.concurrent.TimeoutException { throw new RuntimeException(); }
    public AccessibilityNodeInfo getRootInActiveWindow() { throw new RuntimeException(); }
    public List<AccessibilityWindowInfo> getWindows() { throw new RuntimeException(); }
    public AccessibilityServiceInfo getServiceInfo() { throw new RuntimeException(); }
    public void setServiceInfo(AccessibilityServiceInfo info) { throw new RuntimeException(); }
}
