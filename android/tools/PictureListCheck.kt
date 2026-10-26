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
    val switched = IosPictureList.parse("480\t320\n!\tfade\t250\tunder\n" + line(0, "/c.png") + "\n")!!
    check(switched.fade?.millis == 250L && switched.fade?.over == false, "a new screen's fade")
    check(switched.frames.single().lines.single().path == "/c.png", "the fade line isn't a picture")
    check(still.fade == null, "no fade unless asked")
    println("picture list checks passed")
}
