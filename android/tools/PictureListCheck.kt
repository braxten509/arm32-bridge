// Desktop check of IosPictureList (tools/check-picture-list.sh): a list with
// coming frames is read frame by frame and each moment gets the right frame.
import dev.braxten.gamebridge.IosPictureList
import dev.braxten.gamebridge.LiveScenes
import dev.braxten.gamebridge.PresentLead

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
    // The seam gauge's card box (IOS_DRAW_LOG=1) goes with its own frame; "none" reads empty.
    val boxed = IosPictureList.parse("480\t320\t1000\n!\tcard\t1\t2\t3\t4\n" + line(0, "/a.png") + "\n@\t2000\n!\tcard\tnone\n@\t3000\n")!!
    check(boxed.frames.map { it.card?.toList() } == listOf(listOf(1f, 2f, 3f, 4f), emptyList(), null), "card boxes: ${boxed.frames.map { it.card?.toList() }}")
    // A turned layer's cut (IOS_PHONE_ANIM=1): its corners, its pictures, its end; marks aren't files.
    val cut = IosPictureList.parse("480\t320\n!\tcut\t68\t36\t121\t59\t121\t141\t68\t164\n" + line(0, "/a.png") + "\n!\tuncut\n")!!
    val cutLines = cut.frames.single().lines
    check(cutLines.map { it.mark ?: it.path } == listOf("cut", "/a.png", "uncut"), "cut in order: ${cutLines.map { it.mark ?: it.path }}")
    check(cutLines[0].numbers.toList() == listOf(68f, 36f, 121f, 59f, 121f, 141f, 68f, 164f), "the cut's corners: ${cutLines[0].numbers.toList()}")
    check(cut.paths() == setOf("/a.png"), "a cut isn't a file: ${cut.paths()}")
    // A live scene (IOS_PHONE_ANIM=1): its number, its files in order (decoded before it goes up,
    // hidden ones too), and the phone's presentation clock.
    val live = IosPictureList.parse("480\t320\n" + line(0, "/a.png") + "\n!\tlive\t7\n!\tfile\t/a.png\n!\tfile\t/face.png\n!\tclock\tpresent\n")!!
    check(live.live == 7L && live.files == listOf("/a.png", "/face.png") && live.presentClock, "live: ${live.live} ${live.files} ${live.presentClock}")
    check(live.paths() == setOf("/a.png", "/face.png"), "a scene's files are decoded first: ${live.paths()}")
    check(still.live == null && !still.presentClock, "a plain list isn't live")
    // A live frame as the game hands it over (fw/pictures/live.rs frame_bytes): a cut, a picture
    // with perspective drawn from the scene's second file, the cut's end.
    val bytes = java.nio.ByteBuffer.allocateDirect(512).order(java.nio.ByteOrder.LITTLE_ENDIAN)
    bytes.putInt(1).putInt(3).putInt(1)
    floatArrayOf(1f, 2f, 3f, 4f).forEach { bytes.putFloat(it) }
    bytes.put(1)
    floatArrayOf(68f, 36f, 121f, 59f, 121f, 141f, 68f, 164f).forEach { bytes.putFloat(it) }
    bytes.put(0).putInt(1).put(2)
    (1..19).forEach { bytes.putFloat(it.toFloat()) }
    bytes.put(2)
    val drawn = IosPictureList.liveFrame(bytes, bytes.position(), live.files)!!
    check(drawn.lines.map { it.mark ?: it.path } == listOf("cut", "/face.png", "uncut"), "live frame: ${drawn.lines.map { it.mark ?: it.path }}")
    check(drawn.lines[1].numbers.size == 19 && drawn.lines[1].numbers[18] == 19f && drawn.lines[0].numbers[7] == 164f, "its numbers")
    check(drawn.moving && drawn.card?.toList() == listOf(1f, 2f, 3f, 4f), "moving, with the card box")
    check(IosPictureList.liveFrame(bytes, 20, live.files) == null, "a short frame is refused")
    // Scenes are let go (the game frees their frames and files) only once neither shown nor on
    // their way: a list dropped unshown, the shown one when another (live or not) goes up.
    val released = ArrayList<Long>()
    val scenes = LiveScenes { released.add(it) }
    scenes.read(5); scenes.up(5); scenes.read(6); scenes.read(7)
    check(released.isEmpty(), "shown and pending scenes are kept: $released")
    scenes.dropped(6)
    check(released == listOf(6L), "a dropped one goes: $released")
    scenes.up(7)
    check(released == listOf(6L, 5L), "the one shown before goes once a newer is up: $released")
    scenes.up(null)
    check(released == listOf(6L, 5L, 7L), "a baked list up lets the last scene go: $released")
    // When the view goes, a scene still on its way (being read or its files decoded) is let go
    // only once its list is done with (up or dropped); one arriving after is let go at once.
    released.clear()
    scenes.read(9); scenes.up(9); scenes.read(10); scenes.read(11)
    scenes.close()
    check(released == listOf(9L), "the shown one goes, those on their way stay: $released")
    scenes.dropped(10)
    scenes.up(11)
    check(released == listOf(9L, 10L, 11L), "each once its list is done with: $released")
    scenes.read(12)
    check(released == listOf(9L, 10L, 11L, 12L), "one arriving after goes at once: $released")
    // How far ahead of its drawing a frame shows (IOS_ANIM_LEAD=1): the median of a window of
    // frames, told when it moves by more than 1 ms.
    val told = ArrayList<Long>()
    val lead = PresentLead { told.add(it) }
    repeat(PresentLead.WINDOW - 1) { lead.drawn(26_000_000L) }
    check(told.isEmpty(), "not before a window of frames: $told")
    lead.drawn(90_000_000L)
    check(told == listOf(26_000_000L), "the median, past a slow frame: $told")
    repeat(PresentLead.WINDOW) { lead.drawn(26_600_000L) }
    check(told == listOf(26_000_000L), "under 1 ms isn't told: $told")
    repeat(PresentLead.WINDOW) { lead.drawn(28_000_000L) }
    check(told == listOf(26_000_000L, 28_000_000L), "a change is: $told")
    // A list's scene is known on receipt, before it is read.
    val liveText = "480\t320\t1000\n" + line(0, "/a.png") + "\n!\tlive\t42\n!\tfile\t/a.png\n"
    check(IosPictureList.liveScene(liveText) == 42L && IosPictureList.parse(liveText)!!.live == 42L, "the scene on receipt: ${IosPictureList.liveScene(liveText)}")
    check(IosPictureList.liveScene("480\t320\n" + line(0, "/a.png") + "\n") == null, "a baked list has none")
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

    // Numbers as the iPhone layer writes them (pictures.rs push_number: millionths, no
    // exponent; "inf"/"NaN" only from {v}, whose lines are left out).
    val forms = listOf("0", "-0.5", "12", "1234.000001", "-7.25", "0.333333", "480", "-2048.5", "99999999.999999")
    for (text in forms) {
        val read = IosPictureList.parse("480\t320\n" + line(0, "/a.png").replaceFirst("1\t0\t0\t1\t0", "1\t0\t0\t1\t$text") + "\n")!!
        val n = read.frames.single().lines.single().numbers[4]
        check(Math.abs(n - text.toFloat()) <= Math.ulp(text.toFloat()), "$text read as $n")
    }
    check(IosPictureList.parse("480\t320\n" + line(0, "/a.png").replaceFirst("\t0\t10", "\tinf\t10") + "\n")!!.frames.single().lines.isEmpty(), "a line with inf is left out")
    check(IosPictureList.parse("480\t320\n" + line(0, "/a.png").replaceFirst("\t0\t10", "\t1.2.3\t10") + "\n")!!.frames.single().lines.isEmpty(), "a line with a bad number is left out")
    // Reading a room's list (40 pictures, 37 frames) must take a few ms on the phone (it held the
    // screen 10-36 ms a list): here, under 4 ms a list once the JVM has warmed up.
    val big = StringBuilder("2424\t1080\t1000\n")
    for (k in 0 until 37) {
        if (k > 0) big.append("@\t${1000 + k * 8_333_333L}\n")
        for (j in 0 until 40) big.append("0.999847\t0.017452\t-0.017452\t0.999847\t${100 + j * 13.123457 + k * 0.5}\t${200.25 + j}\t160.5\t213.333333\t1\t0\t320\t427\t0\t0\t568\t320\t/data/pictures/p$j.png\n")
    }
    val text = big.toString()
    repeat(30) { IosPictureList.parse(text) }
    val runs = (0 until 20).map { val t = System.nanoTime(); IosPictureList.parse(text); (System.nanoTime() - t) / 1e6 }.sorted()
    println("room list read: median ${"%.2f".format(runs[10])} ms, best ${"%.2f".format(runs[0])} ms")
    check(runs[10] < 4.0, "a room's list read in ${"%.1f".format(runs[10])} ms (median of 20)")
}
