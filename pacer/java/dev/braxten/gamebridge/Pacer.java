package dev.braxten.gamebridge;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.net.Uri;
import android.util.Log;

/**
 * Loads Game Bridge's frame helper (libgamebridge_pacer.so) into a 64-bit game.
 * The converter adds this class to the game and lists it in the game's manifest
 * as a content provider: Android creates providers before the game's
 * Application.onCreate and its first screen, so the helper is in place before
 * the game loads its own libraries. It answers no queries.
 */
public final class Pacer extends ContentProvider {
    static {
        try {
            System.loadLibrary("gamebridge_pacer");
        } catch (Throwable failure) {
            // Without the helper the game still runs, just with its own frame limits.
            Log.w("gamebridge", "[pacer] not loaded: " + failure);
        }
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] arguments, String order) {
        return null;
    }

    @Override
    public String getType(Uri uri) {
        return null;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] arguments) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] arguments) {
        return 0;
    }
}
