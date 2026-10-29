package dev.braxten.gamebridge

/**
 * The picture list the iPhone layer sends (ios/src/fw/pictures.rs encode and
 * encode_frames), read without Android classes so the desktop JVM can check it
 * (tools/check-picture-list.sh).
 *
 * First line: the screen's width and height in points, and, when views are
 * moving, the first frame's time. Then one line per picture, back to front.
 * While views move, more frames follow, each after a line "@" and its time:
 * where everything is at that moment. Times are CLOCK_MONOTONIC nanoseconds,
 * as System.nanoTime. A line "!", "gl" and a number (0 to 1) or "off" says how
 * much of the OpenGL picture under the pictures shows: as an iPhone shows a GL
 * layer's frame only while that layer is shown, the rest is covered with the
 * window's black. After a frame's "@" line it is that frame's own amount (a GL
 * view fading).
 *
 * Where a layer runs a CATransition, its pictures come as a line "!",
 * "transition" and how it runs (see [Transition]), the pictures it showed
 * before, a line "!", "to", the pictures it shows now, and a line "!", "end".
 *
 * Under a layer turned in 3D that clips to its bounds (IOS_PHONE_ANIM=1), a line "!", "cut" and
 * its bounds' four corners on screen (x, y each, in points), the pictures it holds, and a line
 * "!", "uncut": they are cut to those corners where they are drawn.
 */
class IosPictureList(
    val screenWidth: Float,
    val screenHeight: Float,
    val frames: List<Frame>,
    /** How much of the OpenGL picture shows (1 when the list doesn't say). */
    val glShown: Float = 1f,
    /** A line "!", "live" and a number (IOS_PHONE_ANIM=1): the game's scene that number, whose
     * frame at any moment the screen asks the game for (nativeLiveFrame) while it moves. */
    val live: Long? = null,
    /** The scene's files, one line "!", "file" and its path each, in the order its frames number them. */
    val files: List<String> = emptyList(),
    /** A line "!", "clock", "present": frames are drawn for when they will show on the display. */
    val presentClock: Boolean = false,
) {
    /**
     * A CATransition: its type ("fade", "moveIn", "push", "reveal", "oglFlip", "pageCurl",
     * "pageUnCurl") and subtype ("fromLeft" ...; empty for none), how long, when it began, its
     * timing function's control points (x1, y1, x2, y2), its start and end progress, and the
     * layer's place (a, b, c, d, tx, ty: its own points, (0, 0) to its size, to screen points), its
     * size, and the screen area (x, y, width, height in points) that clips it and stays put while
     * it moves: the views around it that clip, and its own bounds when it masks to them.
     */
    class Transition(
        val type: String,
        val subtype: String,
        val millis: Long,
        val start: Long,
        val curve: FloatArray,
        val fromProgress: Float,
        val toProgress: Float,
        val place: FloatArray,
        val width: Float,
        val height: Float,
        val clip: FloatArray? = null,
    ) {
        /** How far it is at `time` (0 to 1, by its timing function, between its start and end progress). */
        fun progress(time: Long): Float {
            val t = if (millis > 0) ((time - start).toDouble() / (millis * 1_000_000.0)).coerceIn(0.0, 1.0) else 1.0
            return (fromProgress + (toProgress - fromProgress) * eased(t)).toFloat()
        }

        /** Whether it has run its time at `time`. */
        fun doneAt(time: Long): Boolean = time - start >= millis * 1_000_000L

        /** The timing function's height `t` across (a cubic from (0, 0) to (1, 1), as Core Animation's). */
        fun eased(t: Double): Double {
            // The search below only comes close to its ends; the curve meets them exactly.
            if (t <= 0.0 || t >= 1.0) return t.coerceIn(0.0, 1.0)
            val (x1, y1, x2, y2) = curve.map(Float::toDouble)
            fun bezier(s: Double, a: Double, b: Double) = 3 * (1 - s) * (1 - s) * s * a + 3 * (1 - s) * s * s * b + s * s * s
            var low = 0.0
            var high = 1.0
            repeat(40) {
                val middle = (low + high) / 2
                if (bezier(middle, x1, x2) < t) low = middle else high = middle
            }
            return bezier((low + high) / 2, y1, y2)
        }
    }

    /** One picture line: its 16 numbers (place a, b, c, d, tx, ty; area width, height; alpha; mode;
     * picture width, height; clip x, y, width, height), 6 more when it is also cut to a rounded
     * rectangle (x, y, width, height, corner width and height radii, in screen points), 3 more
     * last when its place has perspective (a layer turned in 3D: the place's third column), and its file.
     * Or a transition's mark: its start ([transition] set), "to" or "end" (`mark`; no numbers or file). */
    class Line(val numbers: FloatArray, val path: String, val mark: String? = null, val transition: Transition? = null)

    /** What shows from `time` on (0: the list's only frame, always), and how much of the
     * OpenGL picture shows then when the frame says (null: the list's `glShown`). `card`: with the
     * game's IOS_DRAW_LOG=1, a line "!", "card" gives where the layer whose turn began last is in
     * this frame (x0, x1, y0, y1; empty: "none"), which the phone logs as it draws it. */
    class Frame(val time: Long, val lines: List<Line>, val glShown: Float? = null, val card: FloatArray? = null)

    /** Every file the frames draw. */
    fun paths(): Set<String> = frames.flatMapTo(HashSet(files)) { frame -> frame.lines.filter { it.mark == null }.map { it.path } }

    /** The frame showing at `time`: the last one that has begun (the first before any has). */
    fun frameAt(time: Long): Frame = frames.lastOrNull { it.time <= time } ?: frames.first()

    /** Whether a later frame than the one at `time` is still to come. */
    fun movingAt(time: Long): Boolean = frames.size > 1 && frames.last().time > time

    /** The transitions in the first frame. */
    fun transitions(): List<Transition> = frames.firstOrNull()?.lines?.mapNotNull { it.transition }.orEmpty()

    companion object {
        /** Where the game's screen (width x height points) sits in a window of whole pixels:
         *  left, top, right, bottom, fitted in the middle on the same whole pixels as its GL
         *  picture (eagl::picture_area). That one's y counts up from the bottom (GL), so an odd
         *  spare pixel goes on top here too. */
        fun place(windowWidth: Int, windowHeight: Int, width: Float, height: Float): List<Float> {
            val scale = minOf(windowWidth / width.toDouble(), windowHeight / height.toDouble())
            val (wide, high) = Math.round(width * scale).toInt() to Math.round(height * scale).toInt()
            val (x, y) = (windowWidth - wide) / 2 to (windowHeight - high) / 2
            val top = windowHeight - (y + high)
            return listOf(x, top, x + wide, top + high).map { it.toFloat() }
        }

        /** The whole pixels of the layer a place's picture is drawn on before it is placed:
         *  Apple draws each frame on a framebuffer of whole pixels and shows it whole. */
        fun layerSize(place: List<Float>): Pair<Int, Int> =
            Math.round(place[2] - place[0]) to Math.round(place[3] - place[1])

        /** A live scene's frame (`moving`: something in it still moves then; `card`: the seam
         * gauge's box, or null). */
        class LiveFrame(val lines: List<Line>, val moving: Boolean, val card: FloatArray?)

        /** A live scene's frame as the game wrote it into `bytes` (`length` of them; ios/src/fw/
         * pictures/live.rs frame_bytes): its pictures as list lines, drawn from `files`. */
        fun liveFrame(bytes: java.nio.ByteBuffer, length: Int, files: List<String>): LiveFrame? = runCatching {
            val b = bytes.duplicate().order(java.nio.ByteOrder.LITTLE_ENDIAN)
            b.position(0)
            b.limit(length)
            if (b.getInt() != 1) return null
            val count = b.getInt()
            val moving = b.getInt() != 0
            val box = FloatArray(4) { b.getFloat() }
            val lines = ArrayList<Line>(count)
            repeat(count) {
                when (b.get().toInt()) {
                    0 -> {
                        val file = b.getInt()
                        val flags = b.get().toInt()
                        val n = FloatArray(16 + (if (flags and 1 != 0) 6 else 0) + (if (flags and 2 != 0) 3 else 0)) { b.getFloat() }
                        lines.add(Line(n, files.getOrNull(file) ?: return null))
                    }
                    1 -> lines.add(Line(FloatArray(8) { b.getFloat() }, "", "cut"))
                    2 -> lines.add(Line(FloatArray(0), "", "uncut"))
                    else -> return null
                }
            }
            LiveFrame(lines, moving, box.takeIf { !it[0].isNaN() })
        }.getOrNull()

        /** The live scene a list holds (its "!", "live" line), found on receipt without reading it. */
        fun liveScene(list: String): Long? {
            val mark = "\n!\tlive\t"
            val at = list.indexOf(mark).takeIf { it >= 0 } ?: return null
            val start = at + mark.length
            val end = list.indexOf('\n', start).let { if (it < 0) list.length else it }
            return list.substring(start, end).toLongOrNull()
        }

        fun parse(list: String): IosPictureList? {
            val lines = list.lines().filter { it.isNotEmpty() }
            val screen = lines.firstOrNull()?.split('\t') ?: return null
            val width = screen.getOrNull(0)?.toFloatOrNull() ?: return null
            val height = screen.getOrNull(1)?.toFloatOrNull() ?: return null
            val frames = ArrayList<Frame>()
            var time = screen.getOrNull(2)?.toLongOrNull() ?: 0L
            var current = ArrayList<Line>()
            var glShown = 1f
            // The frame being read has its own GL amount (only after an "@" line).
            var frameGl: Float? = null
            var card: FloatArray? = null
            var live: Long? = null
            val files = ArrayList<String>()
            var presentClock = false
            for (line in lines.drop(1)) {
                if (line.startsWith("!\tfile\t")) {
                    files.add(line.substring(7))
                    continue
                }
                if (line.startsWith("!\t")) {
                    val f = line.split('\t')
                    when (f.getOrNull(1)) {
                        "live" -> live = f.getOrNull(2)?.toLongOrNull()
                        "clock" -> presentClock = f.getOrNull(2) == "present"
                        "gl" -> {
                            val amount = if (f.getOrNull(2) == "off") 0f else (f.getOrNull(2)?.toFloatOrNull() ?: 1f).coerceIn(0f, 1f)
                            if (frames.isEmpty()) glShown = amount else frameGl = amount
                        }
                        "transition" -> transition(f)?.let { current.add(Line(FloatArray(0), "", "transition", it)) }
                        "to", "end", "uncut" -> current.add(Line(FloatArray(0), "", f[1]))
                        "cut" -> f.drop(2).map { it.toFloatOrNull() ?: Float.NaN }.toFloatArray().takeIf { it.size == 8 && it.none(Float::isNaN) }?.let { current.add(Line(it, "", "cut")) }
                        "card" -> card = f.drop(2).mapNotNull { it.toFloatOrNull() }.toFloatArray().takeIf { it.size == 4 } ?: FloatArray(0)
                    }
                    continue
                }
                if (line.startsWith("@\t")) {
                    frames.add(Frame(time, current, frameGl, card))
                    frameGl = null
                    card = null
                    time = line.substring(2).toLongOrNull() ?: return null
                    current = ArrayList()
                    continue
                }
                // Read in place (no split): a room's list holds tens of thousands of numbers.
                val fields = line.count { it == '\t' } + 1
                if (fields != 17 && fields != 20 && fields != 23 && fields != 26) continue
                val numbers = FloatArray(fields - 1)
                var ok = true
                var from = 0
                for (i in numbers.indices) {
                    val to = line.indexOf('\t', from)
                    val n = number(line, from, to)
                    if (n == null) {
                        ok = false
                        break
                    }
                    numbers[i] = n
                    from = to + 1
                }
                if (ok) current.add(Line(numbers, line.substring(from)))
            }
            frames.add(Frame(time, current, frameGl, card))
            return IosPictureList(width, height, frames, glShown, live, files, presentClock)
        }

        /** The number in `text` from `from` to `to`. The iPhone layer writes [-]digits[.digits]
         * (pictures.rs push_number): read here directly, as Kotlin's toFloatOrNull checks each
         * one with a regular expression first (most of a list's reading time on the phone).
         * Anything else goes through toFloatOrNull. */
        fun number(text: String, from: Int, to: Int): Float? {
            var i = from
            val negative = i < to && text[i] == '-'
            if (negative) i++
            var whole = 0L
            var digits = 0
            var scale = 1L
            var point = false
            while (i < to) {
                val c = text[i]
                if (c == '.' && !point) {
                    point = true
                } else if (c in '0'..'9' && digits < 18) {
                    whole = whole * 10 + (c - '0')
                    digits++
                    if (point) scale *= 10
                } else {
                    return text.substring(from, to).toFloatOrNull()
                }
                i++
            }
            if (digits == 0) return text.substring(from, to).toFloatOrNull()
            val value = (whole.toDouble() / scale.toDouble()).toFloat()
            return if (negative) -value else value
        }

        /** "!", "transition", type, subtype, milliseconds, start, x1, y1, x2, y2, start and end
         * progress, a, b, c, d, tx, ty, width, height, and (newer lists) the clip's x, y, width, height. */
        private fun transition(f: List<String>): Transition? {
            if (f.size < 20) return null
            val n = f.subList(6, 20).map { it.toFloatOrNull() ?: return null }
            val clip = if (f.size >= 24) f.subList(20, 24).map { it.toFloatOrNull() ?: return null }.toFloatArray() else null
            return Transition(
                f[2], f[3], f[4].toLongOrNull() ?: return null, f[5].toLongOrNull() ?: 0L,
                n.subList(0, 4).toFloatArray(), n[4], n[5], n.subList(6, 12).toFloatArray(), n[12], n[13], clip,
            )
        }
    }
}

/**
 * The live scenes the screen may still draw (IOS_PHONE_ANIM=1): the one shown and those of lists
 * read and not yet up. The game keeps each scene's frames and files until told it is let go
 * (`release`): when its list is dropped unshown, or once a newer list (live or not) is up. On the
 * UI thread.
 */
class LiveScenes(private val release: (Long) -> Unit) {
    private var shown: Long? = null
    /** Scenes of lists on their way (received, being read or their files decoded): how many lists. */
    private val pending = HashMap<Long, Int>()
    private var closed = false

    /** A list holding `scene` was received: it holds the scene until it is up or dropped. */
    @Synchronized fun read(scene: Long?) {
        scene ?: return
        if (closed) release(scene) else pending[scene] = (pending[scene] ?: 0) + 1
    }

    /** A list holding `scene` was dropped without going up. */
    @Synchronized fun dropped(scene: Long?) {
        scene ?: return
        if (letGo(scene) && scene != shown && scene !in pending) release(scene)
    }

    /** A list went up, holding `scene` (null: not a live one); after `close`, it is dropped. */
    @Synchronized fun up(scene: Long?) {
        if (closed) return dropped(scene)
        scene?.let { letGo(it) }
        val before = shown
        shown = scene
        if (before != null && before != scene && before !in pending) release(before)
    }

    /** The screen went: the shown scene is let go now, each on its way once its list is done
     * with (its files may still be being decoded), and any received later at once. */
    @Synchronized fun close() {
        closed = true
        shown?.let { if (it !in pending) release(it) }
        shown = null
    }

    private fun letGo(scene: Long): Boolean {
        val count = pending[scene] ?: return false
        if (count > 1) pending[scene] = count - 1 else pending.remove(scene)
        return true
    }
}

/** How far ahead of its drawing a frame shows on the screen (its Choreographer presentation
 * time less when it is drawn), for the game's Core Animation clock (IOS_ANIM_LEAD=1): the median
 * of each window of frames, told when it moves by more than 1 ms (UI thread). */
class PresentLead(private val tell: (Long) -> Unit) {
    private val leads = LongArray(WINDOW)
    private var count = 0
    private var told: Long? = null

    /** A frame drawn `lead` nanoseconds before it shows. */
    fun drawn(lead: Long) {
        if (lead <= 0 || lead > 100_000_000L) return
        leads[count++] = lead
        if (count < WINDOW) return
        count = 0
        val median = leads.sorted()[WINDOW / 2]
        if (told.let { it == null || kotlin.math.abs(median - it) > 1_000_000L }) {
            told = median
            tell(median)
        }
    }

    companion object {
        const val WINDOW = 60
    }
}
