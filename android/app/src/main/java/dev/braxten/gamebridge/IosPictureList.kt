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
 */
class IosPictureList(
    val screenWidth: Float,
    val screenHeight: Float,
    val frames: List<Frame>,
    /** How much of the OpenGL picture shows (1 when the list doesn't say). */
    val glShown: Float = 1f,
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
     * OpenGL picture shows then when the frame says (null: the list's `glShown`). */
    class Frame(val time: Long, val lines: List<Line>, val glShown: Float? = null)

    /** Every file the frames draw. */
    fun paths(): Set<String> = frames.flatMapTo(HashSet()) { frame -> frame.lines.filter { it.mark == null }.map { it.path } }

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
            for (line in lines.drop(1)) {
                if (line.startsWith("!\t")) {
                    val f = line.split('\t')
                    when (f.getOrNull(1)) {
                        "gl" -> {
                            val amount = if (f.getOrNull(2) == "off") 0f else (f.getOrNull(2)?.toFloatOrNull() ?: 1f).coerceIn(0f, 1f)
                            if (frames.isEmpty()) glShown = amount else frameGl = amount
                        }
                        "transition" -> transition(f)?.let { current.add(Line(FloatArray(0), "", "transition", it)) }
                        "to", "end" -> current.add(Line(FloatArray(0), "", f[1]))
                    }
                    continue
                }
                if (line.startsWith("@\t")) {
                    frames.add(Frame(time, current, frameGl))
                    frameGl = null
                    time = line.substring(2).toLongOrNull() ?: return null
                    current = ArrayList()
                    continue
                }
                val f = line.split('\t')
                if (f.size != 17 && f.size != 20 && f.size != 23 && f.size != 26) continue
                val numbers = FloatArray(f.size - 1)
                var ok = true
                for (i in numbers.indices) {
                    val n = f[i].toFloatOrNull()
                    if (n == null) {
                        ok = false
                        break
                    }
                    numbers[i] = n
                }
                if (ok) current.add(Line(numbers, f.last()))
            }
            frames.add(Frame(time, current, frameGl))
            return IosPictureList(width, height, frames, glShown)
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
