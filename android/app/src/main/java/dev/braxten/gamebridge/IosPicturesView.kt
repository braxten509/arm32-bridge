package dev.braxten.gamebridge

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.view.View
import java.io.File
import java.util.concurrent.Executors

/**
 * The iPhone game's UIKit pictures (start buttons, splash pictures) drawn over
 * its OpenGL picture, placed (moved, turned, scaled) as the game placed them in
 * screen points, in the same letterboxed area the game draws in. The iPhone layer sends the list when
 * it changes (ios/src/fw/pictures.rs, read by IosPictureList). Takes no touches: they go to the game.
 *
 * While views move, a list holds their coming frames too: each screen refresh
 * draws the frame for its moment, as Core Animation's render server does, so
 * moving pictures keep the screen's rate even while the game thread is busy.
 * New picture files are decoded on a thread of their own; the last list stays
 * on screen until they are ready.
 */
class IosPicturesView(context: Context) : View(context) {
    private class Picture(
        /** The view's own points to screen points. */
        val place: Matrix,
        val area: RectF, val alpha: Float, val mode: Int, val width: Float, val height: Float,
        /** The screen area (points) it may draw in: its clipping views' bounds. */
        val clip: RectF,
        val path: String,
    )

    private class Frame(val time: Long, val pictures: List<Picture>)

    private var screenWidth = 0f
    private var screenHeight = 0f
    private var frames = emptyList<Frame>()
    /** Decoded pictures by file, least recently used first; null when a file can't be read here. */
    private val bitmaps = LinkedHashMap<String, Bitmap?>(64, 0.75f, true)
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
    private val decoder = Executors.newSingleThreadExecutor { Thread(it, "ios-pictures") }
    /** The newest list's number, and the number of the list shown. */
    private var newest = 0L
    private var shown = 0L
    /** A screen fading out (or under a screen fading in): its pictures, when the fade began, how long, which way. */
    private var leaving = emptyList<Picture>()
    private var fadeStart = 0L
    private var fadeNanos = 0L
    private var fadeOver = false

    /** The list from the iPhone layer (on the UI thread). */
    fun show(list: String) {
        // After the view left its window the decoder is shut down; lists still on their way are dropped.
        if (decoder.isShutdown) return
        val parsed = IosPictureList.parse(list) ?: return
        val number = ++newest
        val missing = parsed.paths().filter { it !in bitmaps }
        if (missing.isEmpty()) {
            use(number, parsed)
            return
        }
        decoder.execute {
            val decoded = missing.associateWith { decode(it) }
            post {
                bitmaps.putAll(decoded)
                // A newer list may have gone up meanwhile: only its pictures are kept.
                use(number, parsed)
            }
        }
    }

    private fun decode(path: String): Bitmap? = runCatching {
        // Most of a game's PNGs are Apple's crushed kind, which Android can't read as they are.
        val bytes = ApplePng.readable(File(path))
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.also { it.prepareToDraw() }
    }.getOrNull()

    private fun use(number: Long, list: IosPictureList) {
        if (number <= shown) return
        shown = number
        val fade = list.fade
        if (fade != null && frames.isNotEmpty()) {
            val now = System.nanoTime()
            leaving = frameAt(now).pictures
            fadeStart = now
            fadeNanos = fade.millis * 1_000_000L
            fadeOver = fade.over
        }
        screenWidth = list.screenWidth
        screenHeight = list.screenHeight
        frames = list.frames.map { frame -> Frame(frame.time, frame.lines.map(::picture)) }
        trim(list.paths() + leaving.map { it.path })
        invalidate()
    }

    private fun frameAt(time: Long): Frame = frames.lastOrNull { it.time <= time } ?: frames.first()

    private fun picture(line: IosPictureList.Line): Picture {
        val n = line.numbers
        // a, b, c, d, tx, ty as Apple writes them: x' = a x + c y + tx, y' = b x + d y + ty.
        val place = Matrix().apply { setValues(floatArrayOf(n[0], n[2], n[4], n[1], n[3], n[5], 0f, 0f, 1f)) }
        val clip = RectF(n[12], n[13], n[12] + n[14], n[13] + n[15])
        return Picture(place, RectF(0f, 0f, n[6], n[7]), n[8], n[9].toInt(), n[10], n[11], clip, line.path)
    }

    /** Lets go of the least recently used pictures not in use beyond KEPT_BYTES. */
    private fun trim(inUse: Set<String>) {
        var bytes = bitmaps.values.sumOf { it?.allocationByteCount?.toLong() ?: 0L }
        val iterator = bitmaps.entries.iterator()
        while (bytes > KEPT_BYTES && iterator.hasNext()) {
            val (path, bitmap) = iterator.next()
            if (path in inUse) continue
            bytes -= bitmap?.allocationByteCount?.toLong() ?: 0L
            iterator.remove()
        }
    }

    override fun onDraw(canvas: Canvas) {
        if (frames.isEmpty() || screenWidth <= 0f || screenHeight <= 0f) return
        // The frame for when this one reaches the screen, about a refresh from now.
        val time = System.nanoTime() + refreshNanos()
        val frame = frameAt(time)
        if (frames.size > 1 && frames.last().time > time) postInvalidateOnAnimation()
        // How far a screen fade is (1: done).
        val faded = if (fadeNanos > 0) ((time - fadeStart).toFloat() / fadeNanos).coerceIn(0f, 1f) else 1f
        if (faded >= 1f) {
            leaving = emptyList()
            fadeNanos = 0
        } else {
            postInvalidateOnAnimation()
        }
        val layers = when {
            faded >= 1f -> listOf(frame.pictures to 1f)
            fadeOver -> listOf(leaving to 1f, frame.pictures to faded)
            else -> listOf(frame.pictures to 1f, leaving to 1f - faded)
        }
        if (layers.all { it.first.isEmpty() }) return
        // The game's picture is fitted in the middle of the window (as eagl::picture_area).
        val scale = minOf(width / screenWidth, height / screenHeight)
        val left = (width - screenWidth * scale) / 2f
        val top = (height - screenHeight * scale) / 2f
        canvas.save()
        canvas.clipRect(left, top, left + screenWidth * scale, top + screenHeight * scale)
        for ((pictures, fade) in layers) for (picture in pictures) {
            val bitmap = bitmaps[picture.path] ?: continue
            val area = picture.area
            paint.alpha = (picture.alpha.coerceIn(0f, 1f) * fade * 255).toInt()
            val source = Rect(0, 0, bitmap.width, bitmap.height)
            canvas.save()
            canvas.translate(left, top)
            canvas.scale(scale, scale)
            canvas.clipRect(picture.clip)
            canvas.concat(picture.place)
            // Fit inside, or fill and crop, keep the picture's shape (by its pixels); the rest place it by its points.
            val (w, h) = if (picture.mode == 1 || picture.mode == 2) bitmap.width.toFloat() to bitmap.height.toFloat() else picture.width to picture.height
            val (l, t, r, b) = ContentMode.place(picture.mode, area.width(), area.height(), w, h).toList()
            if (picture.mode == 2) canvas.clipRect(area)
            canvas.drawBitmap(bitmap, source, RectF(area.left + l, area.top + t, area.left + r, area.top + b), paint)
            canvas.restore()
        }
        canvas.restore()
    }

    private fun refreshNanos(): Long {
        val hz = display?.refreshRate ?: 60f
        return (1_000_000_000f / hz.coerceAtLeast(1f)).toLong()
    }

    override fun onDetachedFromWindow() {
        super.onDetachedFromWindow()
        decoder.shutdown()
    }

    private companion object {
        /** Pictures kept for later that no list uses now, at most. */
        const val KEPT_BYTES = 96L shl 20
    }
}
