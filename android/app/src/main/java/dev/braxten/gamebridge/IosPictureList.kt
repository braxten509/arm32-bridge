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
 * as System.nanoTime. A line "!", "fade", milliseconds and "over" or "under"
 * says the list is a new screen in place of the last one: the phone fades
 * from the last screen to it in that time, the new one fading in over the old
 * (over) or the old fading out over the new (under).
 */
class IosPictureList(val screenWidth: Float, val screenHeight: Float, val frames: List<Frame>, val fade: Fade? = null) {
    /** A fade from the last screen: how long, and whether the new screen comes in over the old. */
    class Fade(val millis: Long, val over: Boolean)

    /** One picture line: its 16 numbers (place a, b, c, d, tx, ty; area width, height; alpha; mode;
     * picture width, height; clip x, y, width, height), or 22 when it is also cut to a rounded
     * rectangle (x, y, width, height, corner width and height radii, in screen points), and its file. */
    class Line(val numbers: FloatArray, val path: String)

    /** What shows from `time` on (0: the list's only frame, always). */
    class Frame(val time: Long, val lines: List<Line>)

    /** Every file the frames draw. */
    fun paths(): Set<String> = frames.flatMapTo(HashSet()) { frame -> frame.lines.map { it.path } }

    /** The frame showing at `time`: the last one that has begun (the first before any has). */
    fun frameAt(time: Long): Frame = frames.lastOrNull { it.time <= time } ?: frames.first()

    /** Whether a later frame than the one at `time` is still to come. */
    fun movingAt(time: Long): Boolean = frames.size > 1 && frames.last().time > time

    companion object {
        fun parse(list: String): IosPictureList? {
            val lines = list.lines().filter { it.isNotEmpty() }
            val screen = lines.firstOrNull()?.split('\t') ?: return null
            val width = screen.getOrNull(0)?.toFloatOrNull() ?: return null
            val height = screen.getOrNull(1)?.toFloatOrNull() ?: return null
            val frames = ArrayList<Frame>()
            var time = screen.getOrNull(2)?.toLongOrNull() ?: 0L
            var current = ArrayList<Line>()
            var fade: Fade? = null
            for (line in lines.drop(1)) {
                if (line.startsWith("!\t")) {
                    val f = line.split('\t')
                    if (f.getOrNull(1) == "fade") fade = Fade(f.getOrNull(2)?.toLongOrNull() ?: 0L, f.getOrNull(3) == "over")
                    continue
                }
                if (line.startsWith("@\t")) {
                    frames.add(Frame(time, current))
                    time = line.substring(2).toLongOrNull() ?: return null
                    current = ArrayList()
                    continue
                }
                val f = line.split('\t')
                if (f.size != 17 && f.size != 23) continue
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
            frames.add(Frame(time, current))
            return IosPictureList(width, height, frames, fade?.takeIf { it.millis > 0 })
        }
    }
}
