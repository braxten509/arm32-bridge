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

/**
 * The iPhone game's UIKit pictures (start buttons, splash pictures) drawn over
 * its OpenGL picture, placed (moved, turned, scaled) as the game placed them in
 * screen points, in the same letterboxed area the game draws in. The iPhone layer sends the list when
 * it changes (ios/src/fw/pictures.rs). Takes no touches: they go to the game.
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

    private var screenWidth = 0f
    private var screenHeight = 0f
    private var pictures = emptyList<Picture>()
    /** Decoded pictures by file; null when a file can't be read here. */
    private val bitmaps = HashMap<String, Bitmap?>()
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)

    /** The list from the iPhone layer: screen size in points, then one picture per line. */
    fun show(list: String) {
        val lines = list.lines().filter { it.isNotEmpty() }
        val screen = lines.firstOrNull()?.split('\t') ?: return
        screenWidth = screen.getOrNull(0)?.toFloatOrNull() ?: return
        screenHeight = screen.getOrNull(1)?.toFloatOrNull() ?: return
        pictures = lines.drop(1).mapNotNull { line ->
            val f = line.split('\t', limit = 17)
            if (f.size < 17) return@mapNotNull null
            val n = f.take(16).map { it.toFloatOrNull() ?: return@mapNotNull null }
            // a, b, c, d, tx, ty as Apple writes them: x' = a x + c y + tx, y' = b x + d y + ty.
            val place = Matrix().apply { setValues(floatArrayOf(n[0], n[2], n[4], n[1], n[3], n[5], 0f, 0f, 1f)) }
            val clip = RectF(n[12], n[13], n[12] + n[14], n[13] + n[15])
            Picture(place, RectF(0f, 0f, n[6], n[7]), n[8], n[9].toInt(), n[10], n[11], clip, f[16])
        }
        // Pictures no longer shown are let go of.
        val shown = pictures.map { it.path }.toSet()
        bitmaps.keys.retainAll(shown)
        for (path in shown) {
            // Most of a game's PNGs are Apple's crushed kind, which Android can't read as they are.
            if (path !in bitmaps) bitmaps[path] = runCatching { ApplePng.readable(File(path)).let { BitmapFactory.decodeByteArray(it, 0, it.size) } }.getOrNull()
        }
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        if (pictures.isEmpty() || screenWidth <= 0f || screenHeight <= 0f) return
        // The game's picture is fitted in the middle of the window (as eagl::picture_area).
        val scale = minOf(width / screenWidth, height / screenHeight)
        val left = (width - screenWidth * scale) / 2f
        val top = (height - screenHeight * scale) / 2f
        canvas.save()
        canvas.clipRect(left, top, left + screenWidth * scale, top + screenHeight * scale)
        for (picture in pictures) {
            val bitmap = bitmaps[picture.path] ?: continue
            val area = picture.area
            paint.alpha = (picture.alpha.coerceIn(0f, 1f) * 255).toInt()
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
}
