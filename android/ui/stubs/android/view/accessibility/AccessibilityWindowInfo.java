package android.view.accessibility;

import android.graphics.Rect;

public class AccessibilityWindowInfo {
    public int getId() { throw new RuntimeException(); }
    public int getType() { throw new RuntimeException(); }
    public int getLayer() { throw new RuntimeException(); }
    public CharSequence getTitle() { throw new RuntimeException(); }
    public AccessibilityNodeInfo getRoot() { throw new RuntimeException(); }
    public boolean isActive() { throw new RuntimeException(); }
    public boolean isFocused() { throw new RuntimeException(); }
    public void getBoundsInScreen(Rect r) { throw new RuntimeException(); }
}
