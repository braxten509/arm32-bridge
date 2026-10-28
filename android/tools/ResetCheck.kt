// Desktop check of GameResets (tools/check-resets.sh): a reset moves an iPhone game's saves
// aside as one undo copy, and Undo puts them back.
import dev.braxten.gamebridge.GameResets
import java.io.File
import java.nio.file.Files

fun check(ok: Boolean, what: String) {
    if (!ok) throw AssertionError(what)
}

fun main() {
    val root = Files.createTempDirectory("resets").toFile()
    try {
        val save = File(root, "ios/Game/Documents/save.dat").apply { parentFile.mkdirs(); writeText("level 9") }
        File(root, "ios/Other/Documents/save.dat").apply { parentFile.mkdirs(); writeText("other") }
        check(!GameResets.canUndo(root, "Game"), "nothing to undo before a reset")

        check(GameResets.reset(root, "Game") == null, "reset works")
        check(!save.exists(), "the saves are gone from the game")
        check(GameResets.canUndo(root, "Game"), "a reset can be undone")
        check(File(root, "ios/Other/Documents/save.dat").readText() == "other", "other games keep their saves")

        // Played again after the reset, then reset a second time: the newer saves are the undo copy.
        save.apply { parentFile.mkdirs(); writeText("level 1") }
        check(GameResets.reset(root, "Game") == null, "second reset works")
        check(GameResets.undo(root, "Game") == null, "undo works")
        check(save.readText() == "level 1", "undo brings back the saves from just before the last reset: ${save.takeIf { it.exists() }?.readText()}")
        check(!GameResets.canUndo(root, "Game"), "only one undo")
        check(GameResets.undo(root, "Game") != null, "a second undo is refused")

        check(GameResets.reset(root, "Missing") != null, "a game with no saves has nothing to reset")
        check(GameResets.reset(root, "..") != null && GameResets.reset(root, "a/b") != null, "names that leave the folder are refused")
        println("reset check: all good")
    } finally {
        root.deleteRecursively()
    }
}
