package android.view;

public class MotionEvent extends InputEvent {
    public static final int ACTION_DOWN = 0;
    public static final int ACTION_UP = 1;
    public static MotionEvent obtain(long downTime, long eventTime, int action, float x, float y, float pressure, float size,
            int metaState, float xPrecision, float yPrecision, int deviceId, int edgeFlags) {
        throw new RuntimeException();
    }
    public void setSource(int source) { throw new RuntimeException(); }
}
