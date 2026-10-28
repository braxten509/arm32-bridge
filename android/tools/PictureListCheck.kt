// Desktop check of IosPictureList (tools/check-picture-list.sh): a list with
// coming frames is read frame by frame and each moment gets the right frame.
import dev.braxten.gamebridge.IosPictureList

fun line(tx: Int, path: String) = "1\t0\t0\t1\t$tx\t0\t10\t10\t1\t0\t10\t10\t0\t0\t480\t320\t$path"

fun check(ok: Boolean, what: String) {
    if (!ok) throw AssertionError(what)
}

fun main() {
    val still = IosPictureList.parse("480\t320\n" + line(5, "/a.png") + "\n")!!
    check(still.frames.size == 1 && still.frameAt(123L).lines.single().path == "/a.png", "a list without times is one frame")
    check(!still.movingAt(0L), "nothing moves in it")

    val moving = IosPictureList.parse(
        "480\t320\t1000\n" + line(0, "/a.png") + "\n@\t2000\n" + line(25, "/a.png") + "\n" + line(0, "/b.png") + "\n@\t3000\n" + line(50, "/a.png") + "\n"
    )!!
    check(moving.frames.map { it.time } == listOf(1000L, 2000L, 3000L), "frame times: ${moving.frames.map { it.time }}")
    check(moving.frameAt(500L).time == 1000L, "before the first frame, the first")
    check(moving.frameAt(2500L).lines.first().numbers[4] == 25f, "between frames, the one that began")
    check(moving.frameAt(9000L).time == 3000L, "after the last, the last")
    check(moving.paths() == setOf("/a.png", "/b.png"), "every frame's files: ${moving.paths()}")
    check(moving.movingAt(2500L) && !moving.movingAt(3000L), "moving until the last frame")
    check(IosPictureList.parse("480\t320\n@\tx\n") == null, "a bad time is refused")
    // A GL view fading: the first line's amount for the list (and its first frame), each later frame's own.
    val fading = IosPictureList.parse("480\t320\t1000\n!\tgl\t1\n@\t2000\n!\tgl\t0.5\n@\t3000\n!\tgl\toff\n@\t4000\n")!!
    check(fading.glShown == 1f && fading.frames.map { it.glShown } == listOf(null, 0.5f, 0f, null), "frame GL amounts: ${fading.frames.map { it.glShown }}")
    // A fade that ends on its last frame keeps that frame's amount (no flash back to the first).
    val ended = IosPictureList.parse("480\t320\t1000\n!\tgl\t1\n@\t2000\n!\tgl\t0.5\n@\t3000\n!\tgl\toff\n")!!
    check(ended.frames.map { it.glShown } == listOf(null, 0.5f, 0f), "last frame's GL amount: ${ended.frames.map { it.glShown }}")
    // A layer's CATransition: its line, old pictures, "to", new pictures, "end".
    val start = "!\ttransition\tpush\tfromRight\t500\t1000000000\t0.42\t0\t0.58\t1\t0\t1\t1\t0\t0\t1\t100\t0\t100\t100\t50\t0\t200\t200\n"
    val switched = IosPictureList.parse("480\t320\n" + line(0, "/beside.png") + "\n" + start + line(100, "/old.png") + "\n!\tto\n" + line(100, "/new.png") + "\n!\tend\n")!!
    val lines = switched.frames.single().lines
    check(lines.map { it.mark ?: it.path } == listOf("/beside.png", "transition", "/old.png", "to", "/new.png", "end"), "in order: ${lines.map { it.mark ?: it.path }}")
    check(switched.paths() == setOf("/beside.png", "/old.png", "/new.png"), "marks aren't files: ${switched.paths()}")
    val transition = switched.transitions().single()
    check(transition.type == "push" && transition.subtype == "fromRight" && transition.millis == 500L && transition.start == 1_000_000_000L, "how it runs")
    check(transition.place[4] == 100f && transition.width == 100f && transition.height == 100f, "the layer's place and size")
    check(transition.clip?.toList() == listOf(50f, 0f, 200f, 200f), "the clip that stays put: ${transition.clip?.toList()}")
    check(transition.progress(900_000_000L) == 0f && transition.progress(2_000_000_000L) == 1f, "from 0 before it begins to 1 at its end")
    check(kotlin.math.abs(transition.progress(1_250_000_000L) - 0.5f) < 1e-4f, "halfway along ease-in-ease-out: ${transition.progress(1_250_000_000L)}")
    check(transition.progress(1_100_000_000L) < 0.2f * 0.6f, "ease-in-ease-out starts slowly: ${transition.progress(1_100_000_000L)}")
    check(!transition.doneAt(1_400_000_000L) && transition.doneAt(1_500_000_000L), "done after its time")
    check(still.transitions().isEmpty(), "no transition unless one runs")
    // Both at once: a transition's blocks in every frame, and each later frame's own GL amount.
    val block = start + line(100, "/old.png") + "\n!\tto\n" + line(100, "/new.png") + "\n!\tend\n"
    val both = IosPictureList.parse("480\t320\t1000\n!\tgl\t1\n" + block + "@\t2000\n!\tgl\t0.5\n" + block)!!
    check(both.glShown == 1f && both.frames.map { it.glShown } == listOf(null, 0.5f), "GL amounts beside transitions: ${both.frames.map { it.glShown }}")
    check(both.frames.all { f -> f.lines.map { it.mark ?: it.path } == listOf("transition", "/old.png", "to", "/new.png", "end") }, "every frame keeps its transition")
    check(both.transitions().single().type == "push", "the transition read beside GL lines")
    // The game's place in the window: on the same whole pixels as its GL picture
    // (eagl::picture_area), so no edge column is part-covered by one and whole in the other.
    // SpellCraft's 557x320 at 3.375 is 1879.875 pixels: 272 to 2152.
    val narrow = IosPictureList.place(2424, 1080, 557f, 320f)
    check(narrow == listOf(272f, 0f, 2152f, 1080f), "place: $narrow")
    check(IosPictureList.layerSize(narrow) == (1880 to 1080), "layer: ${IosPictureList.layerSize(narrow)}")
    check(IosPictureList.place(2424, 1080, 568f, 320f) == listOf(253f, 0f, 2170f, 1080f), "568 wide: ${IosPictureList.place(2424, 1080, 568f, 320f)}")
    // The tall way the GL picture's 253 rows below it (GL counts from the bottom) leave 254 above.
    val tall = IosPictureList.place(1080, 2424, 320f, 568f)
    check(tall == listOf(0f, 254f, 1080f, 2171f), "the tall way: $tall")
    println("picture list checks passed")
}
