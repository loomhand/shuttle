package android.view.accessibility;

import android.graphics.Rect;

public class AccessibilityNodeInfo {
    public int getChildCount() { throw new RuntimeException(); }
    public AccessibilityNodeInfo getChild(int i) { throw new RuntimeException(); }
    public CharSequence getText() { throw new RuntimeException(); }
    public CharSequence getContentDescription() { throw new RuntimeException(); }
    public CharSequence getClassName() { throw new RuntimeException(); }
    public String getViewIdResourceName() { throw new RuntimeException(); }
    public void getBoundsInScreen(Rect r) { throw new RuntimeException(); }
    public boolean isVisibleToUser() { throw new RuntimeException(); }
    public boolean isCheckable() { throw new RuntimeException(); }
    public boolean isChecked() { throw new RuntimeException(); }
    public boolean isClickable() { throw new RuntimeException(); }
    public boolean isLongClickable() { throw new RuntimeException(); }
    public boolean isEditable() { throw new RuntimeException(); }
    public boolean isEnabled() { throw new RuntimeException(); }
    public boolean isFocused() { throw new RuntimeException(); }
    public boolean isSelected() { throw new RuntimeException(); }
    public boolean isScrollable() { throw new RuntimeException(); }
    public boolean isShowingHintText() { throw new RuntimeException(); }
    public CharSequence getHintText() { throw new RuntimeException(); }
    public CharSequence getPackageName() { throw new RuntimeException(); }
}
