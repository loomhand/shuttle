package android.view;

public class KeyEvent extends InputEvent {
    public static final int ACTION_DOWN = 0;
    public static final int ACTION_UP = 1;
    public static final int KEYCODE_PASTE = 279;

    public KeyEvent(long downTime, long eventTime, int action, int code, int repeat, int metaState, int deviceId, int scancode,
            int flags, int source) {
        throw new RuntimeException();
    }
}
