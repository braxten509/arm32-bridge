package dev.braxten.gamebridge

import android.app.Activity
import android.app.AlertDialog
import android.os.Environment
import android.os.Looper
import android.os.MessageQueue
import android.os.ParcelFileDescriptor
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
    /** The pipe the "game stopped" byte comes down, kept so it stays open. */
    private var stopPipe: ParcelFileDescriptor? = null

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
        val name = gameName(File(app))
        // Where a report goes when the game stops by mistake, readable without root.
        val reports = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "Game Bridge/logs")
        System.loadLibrary("gamebridge_ios")
        nativeSetReport(name, reports.path)
        if (!nativeStart(holder.surface, app, home, width, height)) {
            finish()
            return
        }
        startedApp = app
        watchForStop(name)
    }

    /** The game's name from its Info.plist, else its folder's. */
    private fun gameName(folder: File): String {
        val info = runCatching { Plist.read(File(folder, "Info.plist")) as? Map<*, *> }.getOrNull()
        fun text(key: String) = (info?.get(key) as? String)?.takeIf { it.isNotBlank() }
        return text("CFBundleDisplayName") ?: text("CFBundleName") ?: folder.nameWithoutExtension
    }

    /**
     * When the game stops by mistake, the iPhone layer saves a report and
     * sends a byte down a pipe; this shows a message over the game, and the
     * process ends when the player closes it.
     */
    private fun watchForStop(name: String) {
        val fd = nativeNoticeFd()
        if (fd < 0) return
        val pipe = ParcelFileDescriptor.adoptFd(fd).also { stopPipe = it }
        Looper.getMainLooper().queue.addOnFileDescriptorEventListener(pipe.fileDescriptor,
            MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT) { descriptor, _ ->
            // 1: the report was saved, 2: it couldn't be.
            val saved = runCatching { java.io.FileInputStream(descriptor).read() }.getOrDefault(-1) == 1
            closeVideo()
            AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
                .setTitle("$name stopped")
                .setMessage("Something went wrong in the game. Your progress up to its last save is kept." +
                    if (saved) " A report was saved in Download/Game Bridge/logs." else "")
                .setCancelable(false)
                .setPositiveButton("OK") { _, _ ->
                    finishAndRemoveTask()
                    android.os.Process.killProcess(android.os.Process.myPid())
                }
                .show()
            0
        }
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
    private external fun nativeNoticeFd(): Int
    private external fun nativeSetReport(name: String, folder: String)
    private external fun nativeResume(surface: android.view.Surface, width: Int, height: Int)
}
