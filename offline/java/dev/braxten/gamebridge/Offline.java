package dev.braxten.gamebridge;

import android.util.Log;

/**
 * The bridge's offline answers (src/offline in libarm32_bridge.so). The game's stand-in
 * library has loaded the bridge already; loading it here binds the native method to it.
 */
public final class Offline {
    private static final boolean LOADED = load();

    private static boolean load() {
        try {
            System.loadLibrary("arm32_bridge");
            return true;
        } catch (Throwable failure) {
            Log.w("arm32bridge", "[offline] bridge not loaded: " + failure);
            return false;
        }
    }

    /**
     * The whole HTTP answer (status line, headers, blank line, body) to a request, or null
     * when the phone has none (the request then fails as with no network).
     */
    public static byte[] answer(String scheme, String host, int port, String method, String uri, String[] headers, byte[] body) {
        if (!LOADED) {
            return null;
        }
        try {
            return nativeAnswer(scheme, host, port, method, uri, headers, body);
        } catch (Throwable failure) {
            Log.w("arm32bridge", "[offline] " + method + " " + host + uri + " failed: " + failure);
            return null;
        }
    }

    private static native byte[] nativeAnswer(String scheme, String host, int port, String method, String uri, String[] headers, byte[] body);

    private Offline() {
    }
}
