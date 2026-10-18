package dev.braxten.gamebridge

import android.app.Activity
import android.os.Bundle
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import java.io.File

/**
 * Runs a 64-bit iPhone game (an unpacked .app folder) full screen. Started with
 * the folder in the "app" extra; the game's own writable files go to this app's
 * files/ios/<folder name>. Runs in its own process, so a game that stops can't
 * take Game Bridge down with it.
 */
class IosGameActivity : Activity(), SurfaceHolder.Callback {
    private var started = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val view = SurfaceView(this)
        view.holder.addCallback(this)
        setContentView(view)
        window.insetsController?.let {
            it.hide(WindowInsets.Type.systemBars())
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        if (started) {
            nativeResize(width, height)
            return
        }
        val app = intent.getStringExtra("app") ?: run { finish(); return }
        val home = File(filesDir, "ios/" + File(app).nameWithoutExtension).path
        System.loadLibrary("gamebridge_ios")
        started = nativeStart(holder.surface, app, home, width, height)
        if (!started) finish()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // The game can't yet carry on without its window: end this process.
        if (started) android.os.Process.killProcess(android.os.Process.myPid())
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!started) return true
        val phase = when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> 0
            MotionEvent.ACTION_MOVE -> 1
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> 3
            else -> 4
        }
        if (phase == 1) {
            for (i in 0 until event.pointerCount) {
                nativeTouch(event.getPointerId(i), 1, event.getX(i), event.getY(i))
            }
        } else if (phase == 4) {
            for (i in 0 until event.pointerCount) {
                nativeTouch(event.getPointerId(i), 4, event.getX(i), event.getY(i))
            }
        } else {
            val i = event.actionIndex
            nativeTouch(event.getPointerId(i), phase, event.getX(i), event.getY(i))
        }
        return true
    }

    private external fun nativeStart(surface: android.view.Surface, app: String, home: String, width: Int, height: Int): Boolean
    private external fun nativeTouch(finger: Int, phase: Int, x: Float, y: Float)
    private external fun nativeResize(width: Int, height: Int)
}
