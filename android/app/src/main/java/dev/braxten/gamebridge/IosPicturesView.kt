package dev.braxten.gamebridge

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BlendMode
import android.graphics.Camera
import android.graphics.Canvas
import android.graphics.LinearGradient
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.RenderNode
import android.graphics.Shader
import android.view.View
import java.io.File
import java.util.concurrent.Executors
import kotlin.math.PI
import kotlin.math.hypot
import kotlin.math.sin

/**
 * The iPhone game's UIKit pictures (start buttons, splash pictures) drawn over
 * its OpenGL picture, placed (moved, turned, scaled) as the game placed them in
 * screen points, in the same letterboxed area the game draws in. The iPhone layer sends the list when
 * it changes (ios/src/fw/pictures.rs, read by IosPictureList). Takes no touches: they go to the game.
 *
 * While views move, a list holds their coming frames too: each screen refresh
 * draws the frame for its moment, as Core Animation's render server does, so
 * moving pictures keep the screen's rate even while the game thread is busy.
 * A layer running a CATransition comes with its old and new pictures, and each
 * refresh draws the transition at its moment (dissolve, slides, flips, page curls).
 * New picture files are decoded on a thread of their own; the last list stays
 * on screen until they are ready.
 */
class IosPicturesView(context: Context) : View(context) {
    private sealed interface Item

    private class Picture(
        /** The view's own points to screen points. */
        val place: Matrix,
        val area: RectF, val alpha: Float, val mode: Int, val width: Float, val height: Float,
        /** The screen area (points) it may draw in: its clipping views' bounds. */
        val clip: RectF,
        /** A rounded rectangle (points) it is also cut to: a layer's round corners (masksToBounds). */
        val corner: Path?,
        val path: String,
    ) : Item

    /** A layer's CATransition: what it showed before and what it shows now. */
    private class Transition(val look: IosPictureList.Transition, val old: List<Item>, val new: List<Item>) : Item {
        /** The layer's own points to screen points. */
        val place = Matrix().apply {
            val p = look.place
            setValues(floatArrayOf(p[0], p[2], p[4], p[1], p[3], p[5], 0f, 0f, 1f))
        }
    }

    /** A frame's pictures and how much of the OpenGL picture shows with them. */
    private class Frame(val time: Long, val items: List<Item>, val gl: Float)

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
    /** The iPhone layer's numbers of the lists not yet up, by their number here. */
    private val ids = HashMap<Long, Long>()
    /** The iPhone layer's number of the newest list up, until a drawn frame says so. */
    private var up: Long? = null
    /** While a transition runs, how much of the OpenGL picture showed when it began (the rest is
     * covered black), and when and how long it runs; each frame says how much shows with it. */
    private var glFrom = 1f
    private var glStart = 0L
    private var glNanos = 0L
    private val cover = Paint().apply { color = android.graphics.Color.BLACK }
    private val camera = Camera()
    /** The layer of whole pixels the game's screen is drawn on (see onDraw). */
    private val screen = RenderNode("iPhone screen")

    /** Told (on the UI thread) the number of each list it puts up, or of one it can't read:
     * the game shows a new OpenGL view's frames from then, under the list that put it up. */
    var onUp: (Long) -> Unit = {}

    /** List `id` from the iPhone layer (on the UI thread). */
    fun show(list: String, id: Long) {
        // After the view left its window the decoder is shut down; lists still on their way are dropped.
        if (decoder.isShutdown) return
        val parsed = IosPictureList.parse(list) ?: return onUp(id)
        val number = ++newest
        ids[number] = id
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
        val id = ids.remove(number)
        if (number <= shown) return
        shown = number
        // Older lists still decoding won't go up; the newest up says for them.
        ids.keys.removeAll { it < number }
        // A transition that began since: the GL picture's cover goes with it from what showed
        // (worked out from the frames shown until now, before they are replaced).
        val now = System.nanoTime()
        list.transitions().maxByOrNull { it.start }?.let { transition ->
            if (transition.start != glStart) {
                glFrom = glAt(now)
                glStart = if (transition.start in 1..now) transition.start else now
                glNanos = transition.millis * 1_000_000L
            }
        }
        screenWidth = list.screenWidth
        screenHeight = list.screenHeight
        frames = list.frames.map { frame -> Frame(frame.time, items(frame.lines.iterator(), top = true), frame.glShown ?: list.glShown) }
        trim(list.paths())
        invalidate()
        // Said once the frame drawing it is handed on to the screen (onDraw), not before:
        // the game's next OpenGL frame would else reach the screen a refresh ahead of it.
        id?.let { up = it }
    }

    /** The items up to a transition's "to" or "end" (or the list's end; at the `top`, one is skipped). */
    private fun items(lines: Iterator<IosPictureList.Line>, top: Boolean = false): List<Item> {
        val out = ArrayList<Item>()
        while (lines.hasNext()) {
            val line = lines.next()
            when (line.mark) {
                null -> out.add(picture(line))
                "transition" -> {
                    val old = items(lines)
                    val new = items(lines)
                    out.add(Transition(line.transition!!, old, new))
                }
                else -> if (!top) return out
            }
        }
        return out
    }

    /** How much of the OpenGL picture shows at `time`: the frame's own amount (a GL view fading
     * frame by frame), reached from `glFrom` over a transition's time while one runs. */
    private fun glAt(time: Long): Float {
        val target = if (frames.isEmpty()) 1f else frameAt(time).gl
        val t = if (glNanos > 0) ((time - glStart).toFloat() / glNanos).coerceIn(0f, 1f) else 1f
        return glFrom + (target - glFrom) * t
    }

    private fun frameAt(time: Long): Frame = frames.lastOrNull { it.time <= time } ?: frames.first()

    private fun picture(line: IosPictureList.Line): Picture {
        val n = line.numbers
        // a, b, c, d, tx, ty as Apple writes them: x' = a x + c y + tx, y' = b x + d y + ty;
        // with perspective (a layer turned in 3D) both divided by p0 x + p1 y + w.
        val perspective = if (n.size == 19 || n.size == 25) floatArrayOf(n[n.size - 3], n[n.size - 2], n[n.size - 1]) else floatArrayOf(0f, 0f, 1f)
        val place = Matrix().apply { setValues(floatArrayOf(n[0], n[2], n[4], n[1], n[3], n[5], perspective[0], perspective[1], perspective[2])) }
        val clip = RectF(n[12], n[13], n[12] + n[14], n[13] + n[15])
        val corner = if (n.size >= 22) {
            Path().apply { addRoundRect(RectF(n[16], n[17], n[16] + n[18], n[17] + n[19]), n[20], n[21], Path.Direction.CW) }
        } else null
        return Picture(place, RectF(0f, 0f, n[6], n[7]), n[8], n[9].toInt(), n[10], n[11], clip, corner, line.path)
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

    /** Whether a transition in `items` still runs at `time`. */
    private fun running(items: List<Item>, time: Long): Boolean =
        items.any { it is Transition && (!it.look.doneAt(time) || running(it.new, time)) }

    override fun onDraw(canvas: Canvas) {
        up?.let { id ->
            up = null
            viewTreeObserver.registerFrameCommitCallback { onUp(id) }
        }
        if (frames.isEmpty() || screenWidth <= 0f || screenHeight <= 0f) return
        // The frame for when this one reaches the screen, about a refresh from now.
        val time = System.nanoTime() + refreshNanos()
        val frame = frameAt(time)
        if (frames.size > 1 && frames.last().time > time || running(frame.items, time) || glNanos > 0 && time - glStart < glNanos) {
            postInvalidateOnAnimation()
        }
        // What of the OpenGL picture doesn't show (its layer left, faded or not drawn yet): black.
        val gl = glAt(time)
        if (frame.items.isEmpty() && gl >= 1f) return
        // The game's picture is fitted in the middle of the window, on its GL picture's whole
        // pixels. As Apple draws each frame on a framebuffer of whole pixels and shows it whole,
        // it is drawn first on a layer of whole pixels, then placed: a fade's own layers then
        // agree with the plain drawing on every edge pixel.
        val place = IosPictureList.place(width, height, screenWidth, screenHeight)
        val (wide, high) = IosPictureList.layerSize(place)
        canvas.save()
        canvas.translate(place[0], place[1])
        if (canvas.isHardwareAccelerated) {
            screen.setPosition(0, 0, wide, high)
            screen.setUseCompositingLayer(true, null)
            val inner = screen.beginRecording(wide, high)
            try {
                drawScreen(inner, wide, high, gl, frame, time)
            } finally {
                screen.endRecording()
            }
            canvas.drawRenderNode(screen)
        } else {
            drawScreen(canvas, wide, high, gl, frame, time)
        }
        canvas.restore()
    }

    /** The game's screen on `wide` x `high` whole pixels from its top left corner. */
    private fun drawScreen(canvas: Canvas, wide: Int, high: Int, gl: Float, frame: Frame, time: Long) {
        canvas.clipRect(0, 0, wide, high)
        if (gl < 1f) {
            cover.alpha = ((1f - gl) * 255).toInt()
            canvas.drawRect(0f, 0f, wide.toFloat(), high.toFloat(), cover)
        }
        // From here on, screen points.
        canvas.scale(wide / screenWidth, high / screenHeight)
        draw(canvas, frame.items, 1f, time)
    }

    private fun draw(canvas: Canvas, items: List<Item>, alpha: Float, time: Long) {
        for (item in items) when (item) {
            is Picture -> draw(canvas, item, alpha)
            is Transition -> draw(canvas, item, alpha, time)
        }
    }

    private fun draw(canvas: Canvas, picture: Picture, alpha: Float) {
        val bitmap = bitmaps[picture.path] ?: return
        val area = picture.area
        paint.alpha = (picture.alpha.coerceIn(0f, 1f) * alpha * 255).toInt()
        val source = Rect(0, 0, bitmap.width, bitmap.height)
        canvas.save()
        canvas.clipRect(picture.clip)
        picture.corner?.let { canvas.clipPath(it) }
        canvas.concat(picture.place)
        // Fit inside, or fill and crop, keep the picture's shape (by its pixels); the rest place it by its points.
        val (w, h) = if (picture.mode == 1 || picture.mode == 2) bitmap.width.toFloat() to bitmap.height.toFloat() else picture.width to picture.height
        val (l, t, r, b) = ContentMode.place(picture.mode, area.width(), area.height(), w, h).toList()
        if (picture.mode == 2) canvas.clipRect(area)
        canvas.drawBitmap(bitmap, source, RectF(area.left + l, area.top + t, area.left + r, area.top + b), paint)
        canvas.restore()
    }

    /**
     * A transition at `time`, as Core Animation draws its types in the layer's own plane:
     * fade blends old and new (old·(1-p) + new·p); moveIn slides the new over the old from the
     * subtype's side, push slides both, reveal slides the old away off the new; UIKit's oglFlip turns
     * the layer half a turn in 3D about its middle (old on the front, new on the back); pageCurl
     * peels the old page up from its bottom corner off the new, pageUnCurl lays the new one down.
     */
    private fun draw(canvas: Canvas, transition: Transition, alpha: Float, time: Long) {
        val look = transition.look
        if (look.doneAt(time)) {
            draw(canvas, transition.new, alpha, time)
            return
        }
        val p = look.progress(time).coerceIn(0f, 1f)
        val (w, h) = look.width to look.height
        // What clips the layer from around it (and its own bounds when it masks to them) stays where it
        // is: what slides or turns is cut by it, not carried along with it.
        canvas.save()
        look.clip?.let { canvas.clipRect(it[0], it[1], it[0] + it[2], it[1] + it[3]) }
        drawMoving(canvas, transition, p, w, h, alpha, time)
        canvas.restore()
    }

    private fun drawMoving(canvas: Canvas, transition: Transition, p: Float, w: Float, h: Float, alpha: Float, time: Long) {
        val look = transition.look
        // Where the new pictures come from, in the layer's points (no subtype: from the left).
        val (dx, dy) = when (look.subtype) {
            "fromRight" -> w to 0f
            "fromTop" -> 0f to -h
            "fromBottom" -> 0f to h
            else -> -w to 0f
        }
        when (look.type) {
            "moveIn" -> {
                draw(canvas, transition.old, alpha, time)
                moved(canvas, transition, dx * (1 - p), dy * (1 - p)) { draw(canvas, transition.new, alpha, time) }
            }
            "push" -> {
                moved(canvas, transition, -dx * p, -dy * p) { draw(canvas, transition.old, alpha, time) }
                moved(canvas, transition, dx * (1 - p), dy * (1 - p)) { draw(canvas, transition.new, alpha, time) }
            }
            "reveal" -> {
                draw(canvas, transition.new, alpha, time)
                moved(canvas, transition, -dx * p, -dy * p) { draw(canvas, transition.old, alpha, time) }
            }
            "oglFlip" -> flipped(canvas, transition, p, alpha, time)
            "pageCurl" -> {
                draw(canvas, transition.new, alpha, time)
                curled(canvas, transition, transition.old, p, alpha, time)
            }
            "pageUnCurl" -> {
                draw(canvas, transition.old, alpha, time)
                curled(canvas, transition, transition.new, 1 - p, alpha, time)
            }
            else -> {
                // The dissolve (and types not drawn here): old·(1-p) + new·p, as one layer.
                canvas.saveLayer(null, Paint().apply { this.alpha = (alpha * 255).toInt() })
                canvas.saveLayer(null, Paint().apply { this.alpha = ((1 - p) * 255).toInt() })
                draw(canvas, transition.old, 1f, time)
                canvas.restore()
                canvas.saveLayer(null, Paint().apply { this.alpha = (p * 255).toInt(); blendMode = BlendMode.PLUS })
                draw(canvas, transition.new, 1f, time)
                canvas.restore()
                canvas.restore()
            }
        }
    }

    /** Draws with `local` (a map in the layer's own points) applied in screen points. */
    private inline fun inLayer(canvas: Canvas, transition: Transition, local: Matrix, drawing: () -> Unit) {
        val back = Matrix()
        transition.place.invert(back)
        val screen = Matrix(back).apply {
            postConcat(local)
            postConcat(transition.place)
        }
        canvas.save()
        canvas.concat(screen)
        drawing()
        canvas.restore()
    }

    private inline fun moved(canvas: Canvas, transition: Transition, x: Float, y: Float, drawing: () -> Unit) =
        inLayer(canvas, transition, Matrix().apply { setTranslate(x, y) }, drawing)

    /** Half a turn about the layer's middle (sideways for fromLeft and fromRight, else up or down), with
     * perspective; the old pictures on the front half of the way, then the new ones on the back. */
    private fun flipped(canvas: Canvas, transition: Transition, p: Float, alpha: Float, time: Long) {
        val look = transition.look
        val degrees = if (p < 0.5f) 180f * p else 180f * (p - 1f)
        val sign = if (look.subtype == "fromRight" || look.subtype == "fromBottom") -1f else 1f
        val turn = Matrix()
        camera.save()
        // Apple's flip backs away a little as it turns.
        camera.translate(0f, 0f, 0.25f * maxOf(look.width, look.height) * sin(PI * p).toFloat())
        if (look.subtype == "fromTop" || look.subtype == "fromBottom") camera.rotateX(-sign * degrees) else camera.rotateY(sign * degrees)
        camera.getMatrix(turn)
        camera.restore()
        turn.preTranslate(-look.width / 2, -look.height / 2)
        turn.postTranslate(look.width / 2, look.height / 2)
        inLayer(canvas, transition, turn) { draw(canvas, if (p < 0.5f) transition.old else transition.new, alpha, time) }
    }

    /**
     * `page` peeled `amount` (0 flat, 1 gone) from the layer's bottom right corner: the flat part
     * up to a fold line, then the page's back rolled over it (a light, faintly see-through copy
     * mirrored at the fold) with shading, and a soft shadow on what shows under the peeled part.
     */
    private fun curled(canvas: Canvas, transition: Transition, page: List<Item>, amount: Float, alpha: Float, time: Long) {
        if (amount >= 1f) return
        val (w, h) = transition.look.width to transition.look.height
        if (amount <= 0f || w <= 0f || h <= 0f) {
            draw(canvas, page, alpha, time)
            return
        }
        // The fold line moves across the page against `n` (pointing down and a little right).
        val length = hypot(0.3f, 1f)
        val (nx, ny) = 0.3f / length to 1f / length
        val roll = 0.12f * minOf(w, h)
        val reach = w * nx + h * ny + roll
        // A point on the fold: from the bottom right corner up and left.
        val (fx, fy) = w - nx * reach * amount to h - ny * reach * amount
        val side = { x: Float, y: Float -> (x - fx) * nx + (y - fy) * ny }
        val corners = listOf(0f to 0f, w to 0f, w to h, 0f to h)
        // The flat part: the page where it is before the fold.
        val flat = cut(corners, { x, y -> -side(x, y) })
        clipped(canvas, transition, flat) { draw(canvas, page, alpha, time) }
        // The shadow the peeled part throws on what shows under it.
        val under = cut(corners, { x, y -> side(x, y) })
        clipped(canvas, transition, under) {
            shade(canvas, transition, fx, fy, fx + nx * roll, fy + ny * roll, 0x55000000, 0)
        }
        // The page's back: the part past the fold mirrored over it, as far as it rolls.
        val mirror = Matrix().apply {
            setValues(floatArrayOf(1 - 2 * nx * nx, -2 * nx * ny, 0f, -2 * nx * ny, 1 - 2 * ny * ny, 0f, 0f, 0f, 1f))
            preTranslate(-fx, -fy)
            postTranslate(fx, fy)
        }
        // Within the roll of the fold, and inside the page's mirrored outline.
        val mirrored = { x: Float, y: Float -> floatArrayOf(x, y).also { mirror.mapPoints(it) } }
        var back = cut(flat, { x, y -> side(x, y) + roll })
        back = cut(back, { x, y -> mirrored(x, y)[0] })
        back = cut(back, { x, y -> w - mirrored(x, y)[0] })
        back = cut(back, { x, y -> mirrored(x, y)[1] })
        back = cut(back, { x, y -> h - mirrored(x, y)[1] })
        if (back.size < 3) return
        clipped(canvas, transition, back) {
            inOwnPoints(canvas, transition) {
                canvas.drawRect(0f, 0f, w, h, Paint().apply { color = 0xFFEDEDED.toInt(); this.alpha = (alpha * 255).toInt() })
            }
            inLayer(canvas, transition, mirror) { draw(canvas, page, alpha * 0.25f, time) }
            shade(canvas, transition, fx, fy, fx - nx * roll, fy - ny * roll, 0x40000000, 0x10FFFFFF)
        }
    }

    /** A gradient across the layer from (x0, y0) to (x1, y1), in its own points. */
    private fun shade(canvas: Canvas, transition: Transition, x0: Float, y0: Float, x1: Float, y1: Float, from: Int, to: Int) {
        val shader = LinearGradient(x0, y0, x1, y1, from, to, Shader.TileMode.CLAMP)
        inOwnPoints(canvas, transition) {
            canvas.drawRect(0f, 0f, transition.look.width, transition.look.height, Paint().apply { this.shader = shader })
        }
    }

    /** Draws in the layer's own points ((0, 0) to its size). */
    private inline fun inOwnPoints(canvas: Canvas, transition: Transition, drawing: () -> Unit) {
        canvas.save()
        canvas.concat(transition.place)
        drawing()
        canvas.restore()
    }

    /** Draws cut to `shape` (the layer's own points). */
    private inline fun clipped(canvas: Canvas, transition: Transition, shape: List<Pair<Float, Float>>, drawing: () -> Unit) {
        if (shape.size < 3) return
        canvas.save()
        canvas.concat(transition.place)
        canvas.clipPath(path(shape))
        val back = Matrix()
        transition.place.invert(back)
        canvas.concat(back)
        drawing()
        canvas.restore()
    }

    private fun path(shape: List<Pair<Float, Float>>) = Path().apply {
        shape.forEachIndexed { i, (x, y) -> if (i == 0) moveTo(x, y) else lineTo(x, y) }
        close()
    }

    /** The part of a convex polygon where `inside` is 0 or more (one Sutherland-Hodgman step). */
    private fun cut(shape: List<Pair<Float, Float>>, inside: (Float, Float) -> Float): List<Pair<Float, Float>> {
        val out = ArrayList<Pair<Float, Float>>()
        for (i in shape.indices) {
            val (ax, ay) = shape[i]
            val (bx, by) = shape[(i + 1) % shape.size]
            val (da, db) = inside(ax, ay) to inside(bx, by)
            if (da >= 0) out.add(ax to ay)
            if (da >= 0 != db >= 0) {
                val t = da / (da - db)
                out.add(ax + (bx - ax) * t to ay + (by - ay) * t)
            }
        }
        return out
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
