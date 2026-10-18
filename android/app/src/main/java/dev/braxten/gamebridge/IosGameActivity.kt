package dev.braxten.gamebridge

import android.app.Activity
import android.os.Bundle
import android.graphics.Color
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.VideoView
import java.io.File

/**
 * Runs a 64-bit iPhone game (an unpacked .app folder) full screen. Started with
 * the folder in the "app" extra; the game's own writable files go to this app's
 * files/ios/<folder name>. Runs in its own process, so a game that stops can't
 * take Game Bridge down with it.
 */
class IosGameActivity : Activity(), SurfaceHolder.Callback {
    companion object {
        /** The game running in this process (one per process), or null. */
        private var startedApp: String? = null
        /** The player left and the game let go of its window; it waits for a new one. */
        private var away = false
    }
    private val started get() = startedApp != null
    private lateinit var root: FrameLayout
    /** The game's video on screen (over the game, black around it), or null. */
    private var video: FrameLayout? = null
    private var videoPlayer: VideoView? = null
    /** The game's number for the video on screen (0: none). */
    private var videoToken = 0L

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (switchingGames(intent)) return
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val view = SurfaceView(this)
        view.holder.addCallback(this)
        root = FrameLayout(this)
        root.addView(view)
        setContentView(root)
        window.insetsController?.let {
            it.hide(WindowInsets.Type.systemBars())
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        switchingGames(intent)
    }

    /** Another game asked for while one runs here: start this process over for it. */
    private fun switchingGames(intent: android.content.Intent): Boolean {
        val app = intent.getStringExtra("app") ?: return false
        if (startedApp == null || startedApp == app) return false
        startActivity(intent)
        android.os.Process.killProcess(android.os.Process.myPid())
        return true
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        if (started) {
            if (away) {
                away = false
                nativeResume(holder.surface, width, height)
            } else {
                nativeResize(width, height)
            }
            return
        }
        val app = intent.getStringExtra("app") ?: run { finish(); return }
        val home = File(filesDir, "ios/" + File(app).nameWithoutExtension).path
        System.loadLibrary("gamebridge_ios")
        if (nativeStart(holder.surface, app, home, width, height)) startedApp = app else finish()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // The game pauses (told it is in the background, like on iOS) and
        // lets go of the window before this returns; it carries on when the
        // player comes back. Android may still close it while away.
        if (started && !away) {
            away = true
            nativePause()
        }
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

    /** Called by the game (any thread): show a video file over the game. */
    @Suppress("unused")
    fun playVideo(path: String, token: Long) {
        runOnUiThread {
            closeVideo()
            // Touches pass through to onTouchEvent, so the game can skip it.
            val frame = FrameLayout(this)
            frame.setBackgroundColor(Color.BLACK)
            val player = VideoView(this)
            player.setZOrderMediaOverlay(true)
            val ended = {
                if (videoToken == token) closeVideo()
                nativeVideoEnded(token)
            }
            player.setOnCompletionListener { ended() }
            player.setOnErrorListener { _, _, _ ->
                ended()
                true
            }
            frame.addView(player, FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT, Gravity.CENTER))
            root.addView(frame)
            video = frame
            videoPlayer = player
            videoToken = token
            player.setVideoPath(path)
            player.start()
        }
    }

    /** Called by the game (any thread): hold the video where it is. */
    @Suppress("unused")
    fun pauseVideo(token: Long) {
        runOnUiThread { if (videoToken == token) videoPlayer?.pause() }
    }

    /** Called by the game (any thread): go on from where it was held. */
    @Suppress("unused")
    fun resumeVideo(token: Long) {
        runOnUiThread { if (videoToken == token) videoPlayer?.start() }
    }

    /** Called by the game (any thread): take the video off the screen. */
    @Suppress("unused")
    fun stopVideo(token: Long) {
        runOnUiThread { if (videoToken == token) closeVideo() }
    }

    private fun closeVideo() {
        videoPlayer?.stopPlayback()
        video?.let { root.removeView(it) }
        video = null
        videoPlayer = null
        videoToken = 0
    }

    private external fun nativeVideoEnded(token: Long)

    private external fun nativeStart(surface: android.view.Surface, app: String, home: String, width: Int, height: Int): Boolean
    private external fun nativeTouch(finger: Int, phase: Int, x: Float, y: Float)
    private external fun nativeResize(width: Int, height: Int)
    private external fun nativePause()
    private external fun nativeResume(surface: android.view.Surface, width: Int, height: Int)
}
