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
        /** The list line's numbers (see `picture`). */
        val n: FloatArray,
        val area: RectF, val alpha: Float, val mode: Int, val width: Float, val height: Float,
        /** The screen area (points) it may draw in: its clipping views' bounds. */
        val clip: RectF,
        val path: String,
    ) : Item {
        // Its Matrix is set when drawn and its Path made then, not when the list is read: a list
        // holds a picture for each of its coming frames, and making a native Matrix (and Path)
        // for each was much of a list's reading time on the phone.
        /** Sets `m` to the view's own points to screen points: a, b, c, d, tx, ty as Apple writes
         * them (x' = a x + c y + tx, y' = b x + d y + ty); with perspective (a layer turned in
         * 3D) both divided by p0 x + p1 y + w. */
        fun place(m: Matrix, values: FloatArray) {
            val p = if (n.size == 19 || n.size == 25) n.size - 3 else -1
            values[0] = n[0]; values[1] = n[2]; values[2] = n[4]; values[3] = n[1]; values[4] = n[3]; values[5] = n[5]
            values[6] = if (p < 0) 0f else n[p]; values[7] = if (p < 0) 0f else n[p + 1]; values[8] = if (p < 0) 1f else n[p + 2]
            m.setValues(values)
        }
        /** A rounded rectangle (points) it is also cut to: a layer's round corners (masksToBounds). */
        val corner: Path? by lazy(LazyThreadSafetyMode.NONE) {
            if (n.size >= 22) Path().apply { addRoundRect(RectF(n[16], n[17], n[16] + n[18], n[17] + n[19]), n[20], n[21], Path.Direction.CW) } else null
        }
    }

    /** A layer's CATransition: what it showed before and what it shows now. */
    private class Transition(val look: IosPictureList.Transition, val old: List<Item>, val new: List<Item>) : Item {
        /** The layer's own points to screen points. */
        val place = Matrix().apply {
            val p = look.place
            setValues(floatArrayOf(p[0], p[2], p[4], p[1], p[3], p[5], 0f, 0f, 1f))
        }
    }

    /** Pictures cut to a turned layer's bounds: its four corners on screen (points). */
    private class Cut(corners: FloatArray, val items: List<Item>) : Item {
        val shape = Path().apply {
            moveTo(corners[0], corners[1])
            for (i in 2 until corners.size step 2) lineTo(corners[i], corners[i + 1])
            close()
        }
    }

    /** A frame's pictures and how much of the OpenGL picture shows with them; `card`: the seam
     * gauge's box (IosPictureList.Frame), logged as the frame is drawn. */
    private class Frame(val time: Long, val items: List<Item>, val gl: Float, val card: FloatArray? = null)

    private var screenWidth = 0f
    private var screenHeight = 0f
    private var frames = emptyList<Frame>()
    /** Decoded pictures by file, least recently used first; null when a file can't be read here. */
    private val bitmaps = LinkedHashMap<String, Bitmap?>(64, 0.75f, true)
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
    /** Set to each picture's place as it is drawn (UI thread; the canvas copies it). */
    private val placing = Matrix()
    private val placingValues = FloatArray(9)
    private val decoder = Executors.newSingleThreadExecutor { Thread(it, "ios-pictures") }
    /** Where a list's new pictures are decoded side by side (a new card's face and its parts
     * took ~30 ms one after another, holding the list and the ones behind it). */
    private val decoders = Executors.newFixedThreadPool(3) { Thread(it, "ios-picture-decode") }
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

    /** Writes live scene `scene`'s frame at `time` into the buffer (the game's nativeLiveFrame):
     * its length, -1 when it can't, -2 when the buffer is too small. */
    var liveFrame: (scene: Long, time: Long, into: java.nio.ByteBuffer) -> Int = { _, _, _ -> -1 }

    /** Tells the game a live scene is let go (the game's nativeLiveRelease). */
    var liveRelease: (scene: Long) -> Unit = {}

    /** Tells the game how far ahead of its drawing a frame shows (the game's nativeAnimationLead). */
    var animationLead: (nanos: Long) -> Unit = {}
    /** The presentation leads measured (`PresentLead`), told to the game when they change. */
    private val presentLead = PresentLead { animationLead(it) }

    /** The live scenes shown or on their way: let go only when neither (UI thread). */
    private val scenes = LiveScenes { liveRelease(it) }
    private val main = android.os.Handler(android.os.Looper.getMainLooper())

    /** The live scene of the list up (IOS_PHONE_ANIM=1) and its files; null: the list's own frames. */
    private var liveScene: Long? = null
    private var liveFiles = emptyList<String>()
    private var liveBytes = java.nio.ByteBuffer.allocateDirect(1 shl 16)
    /** Live frames' times to work out and read (nanoseconds), logged now and then while measuring. */
    private val liveNanos = ArrayList<Long>()
    /** The list up draws each frame for when it will show (its "clock present" line). */
    private var presentClock = false
    /** The coming frame's vsync time and when the display will show it (Choreographer's frame data). */
    private var vsyncFrame = 0L
    private var vsyncShows = 0L
    private var vsyncAsked = false

    /** A list read, with its frames' pictures placed and the files it shows. */
    private class Read(val list: IosPictureList, val frames: List<Frame>, val paths: Set<String>)

    /** List `id` from the iPhone layer (on the UI thread). It is read on the decoder's thread (a
     * long list took a few screen refreshes to read, holding up the frames drawn meanwhile); the
     * lists still go up in order, as that thread takes them one at a time. */
    fun show(list: String, id: Long, sent: Long = System.nanoTime()) {
        // Its scene is held from now until the list is up or dropped (after `close`, let go at once).
        val scene = IosPictureList.liveScene(list)
        scenes.read(scene)
        // After the view left its window the decoder is shut down; lists still on their way are dropped.
        if (decoder.isShutdown) return
        val number = ++newest
        ids[number] = id
        val times = Times(sent, System.nanoTime())
        if (measuring) timing[number] = times
        decoder.execute {
            times.parseStart = System.nanoTime()
            val parsed = IosPictureList.parse(list)
            val read = parsed?.let { p ->
                Read(p, p.frames.map { frame -> Frame(frame.time, items(frame.lines.iterator(), top = true), frame.glShown ?: p.glShown, frame.card) }, p.paths())
            }
            times.parsed = System.nanoTime()
            // (The main thread's own queue: a view's post waits for it to be attached again.)
            main.post { up(number, id, read, scene) }
        }
    }

    /** When a list was sent, reached the UI thread, began and ended being read, had its new
     * pictures decoded (how many), went up, was drawn: logged when it took long, to measure. */
    private class Times(val sent: Long, val ui: Long) {
        var parseStart = 0L; var parsed = 0L; var decoded = 0L; var pictures = 0; var used = 0L; var drawn = 0L
    }
    private val timing = java.util.concurrent.ConcurrentHashMap<Long, Times>()
    /** The game's test switch IOS_DRAW_LOG=1 (setprop debug.gamebridge.ios.env) also turns on
     * these timings and the "ran out" lines (logcat GameBridge). */
    private val measuring: Boolean by lazy {
        runCatching {
            Class.forName("android.os.SystemProperties").getMethod("get", String::class.java).invoke(null, "debug.gamebridge.ios.env") as String
        }.getOrNull()?.split(';')?.any { it.trim() == "IOS_DRAW_LOG=1" } == true
    }
    /** The number here of the list `up` stands for. */
    private var drawing = 0L
    /** The iPhone layer's number of the list being drawn (the seam gauge's log). */
    private var drawingId = 0L

    private fun logTimes(number: Long) {
        val t = timing.remove(number) ?: return
        timing.keys.removeAll { it < number }
        val now = System.nanoTime()
        fun ms(a: Long, b: Long) = if (a == 0L || b == 0L) -1.0 else (b - a) / 1e6
        if (now - t.sent > 20_000_000L) {
            android.util.Log.i("GameBridge", "pictures: list $number up %.1f ms after sent: to UI %.1f, queued %.1f, read %.1f, %d new pictures %.1f, to use %.1f, to draw %.1f, to commit %.1f".format(
                (now - t.sent) / 1e6, ms(t.sent, t.ui), ms(t.ui, t.parseStart), ms(t.parseStart, t.parsed), t.pictures,
                if (t.pictures > 0) ms(t.parsed, t.decoded) else 0.0, ms(if (t.decoded != 0L) t.decoded else t.parsed, t.used), ms(t.used, t.drawn), ms(t.drawn, now)))
        }
    }

    /** List `number` read (on the UI thread): up now, or once its new picture files are decoded. */
    private fun up(number: Long, id: Long, read: Read?, scene: Long?) {
        if (read == null) {
            scenes.dropped(scene)
            ids.remove(number)
            onUp(id)
            return
        }
        val missing = read.paths.filter { it !in bitmaps }
        if (missing.isEmpty() || decoder.isShutdown) {
            use(number, read)
            return
        }
        timing[number]?.pictures = missing.size
        decoder.execute {
            val decoded = if (missing.size == 1) mapOf(missing[0] to decode(missing[0]))
                else missing.map { path -> decoders.submit<Bitmap?> { decode(path) } }.let { jobs -> missing.zip(jobs) { path, job -> path to job.get() }.toMap() }
            timing[number]?.decoded = System.nanoTime()
            main.post {
                bitmaps.putAll(decoded)
                // A newer list may have gone up meanwhile: only its pictures are kept.
                use(number, read)
            }
        }
    }

    private fun decode(path: String): Bitmap? = runCatching {
        // Most of a game's PNGs are Apple's crushed kind, which Android can't read as they are.
        val bytes = ApplePng.readable(File(path))
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.also { it.prepareToDraw() }
    }.getOrNull()

    private fun use(number: Long, read: Read) {
        val list = read.list
        val id = ids.remove(number)
        if (number <= shown) {
            scenes.dropped(list.live)
            return
        }
        shown = number
        timing[number]?.used = System.nanoTime()
        drawing = number
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
        // Moving pictures whose coming frames ran out before this list came stood still meanwhile
        // (a card held edge-on between the two halves of its turn): logged, to measure.
        if (measuring && frames.size > 1) {
            val ranOut = (now + refreshNanos() - frames.last().time) / 1e6
            if (ranOut > 0 && ranOut < 500) android.util.Log.i("GameBridge", "pictures: coming frames ran out %.1f ms before list $number".format(ranOut))
        }
        frames = read.frames
        liveScene = list.live
        scenes.up(list.live)
        liveFiles = list.files
        presentClock = list.presentClock
        drawingId = id ?: 0L
        // The seam gauge: when this list went, was read, had its pictures decoded and went up.
        if (measuring && frames.any { it.card != null }) timing[number]?.let { t ->
            android.util.Log.i("GameBridge", "pictures: seam list $drawingId sent ${t.sent} ui ${t.ui} read ${t.parsed} decoded ${t.decoded} pictures ${t.pictures} used ${t.used}")
        }
        trim(read.paths)
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
                "cut" -> out.add(Cut(line.numbers, items(lines)))
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
        val clip = RectF(n[12], n[13], n[12] + n[14], n[13] + n[15])
        return Picture(n, RectF(0f, 0f, n[6], n[7]), n[8], n[9].toInt(), n[10], n[11], clip, line.path)
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
        items.any { it is Transition && (!it.look.doneAt(time) || running(it.new, time)) || it is Cut && running(it.items, time) }

    override fun onDraw(canvas: Canvas) {
        up?.let { id ->
            up = null
            val number = drawing
            timing[number]?.drawn = System.nanoTime()
            viewTreeObserver.registerFrameCommitCallback { logTimes(number); onUp(id) }
        }
        if (frames.isEmpty() || screenWidth <= 0f || screenHeight <= 0f) return
        // The frame for when this one reaches the screen: about a refresh from now, or (the "clock
        // present" lists) when the display will show it, as Choreographer says for this vsync.
        val now = System.nanoTime()
        if (presentClock) askVsync()
        val time = if (presentClock && vsyncShows > 0 && now - vsyncFrame < 2 * refreshNanos()) vsyncShows else now + refreshNanos()
        if (presentClock && time == vsyncShows) presentLead.drawn(time - now)
        val listed = frameAt(time)
        val live = liveScene?.let { scene -> liveFrameAt(scene, time, listed.gl) }
        val frame = live?.first ?: listed
        if (live?.second == true) postInvalidateOnAnimation()
        frame.card?.let { card ->
            if (measuring) android.util.Log.i("GameBridge", "pictures: seam drawn $time at $now list $drawingId card " +
                if (card.size == 4) String.format(java.util.Locale.ROOT, "%.3f %.3f %.3f %.3f", card[0], card[1], card[2], card[3]) else "none")
        }
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
            is Cut -> {
                canvas.save()
                canvas.clipPath(item.shape)
                draw(canvas, item.items, alpha, time)
                canvas.restore()
            }
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
        picture.place(placing, placingValues)
        canvas.concat(placing)
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

    /** The live scene's frame at `time` (and whether something in it still moves), worked out by
     * the game from the scene alone; null when it can't (the list's own frame shows). */
    private fun liveFrameAt(scene: Long, time: Long, gl: Float): Pair<Frame, Boolean>? {
        val start = System.nanoTime()
        var length = liveFrame(scene, time, liveBytes)
        if (length == -2) {
            liveBytes = java.nio.ByteBuffer.allocateDirect(liveBytes.capacity() * 4)
            length = liveFrame(scene, time, liveBytes)
        }
        if (length <= 0) return null
        val read = IosPictureList.liveFrame(liveBytes, length, liveFiles) ?: return null
        val frame = Frame(time, items(read.lines.iterator(), top = true), gl, if (measuring) read.card ?: FloatArray(0) else null)
        if (measuring) {
            liveNanos.add(System.nanoTime() - start)
            if (liveNanos.size >= 240) {
                val sorted = liveNanos.sorted()
                android.util.Log.i("GameBridge", "pictures: live frames worked out and read in %.2f ms median, %.2f p90, %.2f most (%d frames)".format(
                    sorted[sorted.size / 2] / 1e6, sorted[sorted.size * 9 / 10] / 1e6, sorted.last() / 1e6, sorted.size))
                liveNanos.clear()
            }
        }
        return frame to read.moving
    }

    /** Asks Choreographer for the coming frame's data (Android 13 and later): when its vsync is
     * and when the display will show what is drawn for it. */
    private fun askVsync() {
        if (vsyncAsked || android.os.Build.VERSION.SDK_INT < 33) return
        vsyncAsked = true
        android.view.Choreographer.getInstance().postVsyncCallback { data ->
            vsyncAsked = false
            vsyncFrame = data.frameTimeNanos
            vsyncShows = data.preferredFrameTimeline.expectedPresentationTimeNanos
        }
    }

    private fun refreshNanos(): Long {
        val hz = display?.refreshRate ?: 60f
        return (1_000_000_000f / hz.coerceAtLeast(1f)).toLong()
    }

    override fun onDetachedFromWindow() {
        super.onDetachedFromWindow()
        // The decoding pool goes once the lists already given to the decoder are done with it.
        if (!decoder.isShutdown) decoder.execute { decoders.shutdown() }
        decoder.shutdown()
        timing.clear()
        // Scenes of lists still being read or decoded go once each is done with.
        scenes.close()
    }

    private companion object {
        /** Pictures kept for later that no list uses now, at most. */
        const val KEPT_BYTES = 96L shl 20
    }
}
