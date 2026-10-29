// Desktop check of GameSettings (tools/check-settings.sh): each game's settings
// start at its own defaults, a choice is kept in a file (so it outlasts a restart),
// and the game gets the switches that are on.
import dev.braxten.gamebridge.GameSettings
import java.io.File

fun check(ok: Boolean, what: String) {
    if (!ok) throw AssertionError(what)
}

fun main() {
    val root = File(System.getProperty("java.io.tmpdir"), "settings-check-" + System.nanoTime()).apply { mkdirs() }
    try {
        val spellcraft = "com.appyentertainment.spellcraft"
        val keys = GameSettings.settings.map { it.key }
        check(keys.isNotEmpty(), "there are settings")
        check(GameSettings.settings.none { "IOS_PHONE_ANIM" in it.env }, "phone animations are always on, not a setting")
        check(GameSettings.choices(root, spellcraft).values.all { it }, "SpellCraft starts with its switches on: ${GameSettings.choices(root, spellcraft)}")
        check(GameSettings.env(root, spellcraft).split(';').containsAll(listOf("IOS_ANIM_LEAD=1", "IOS_ANIM_LEAD_EXTRA_MS=8")), "SpellCraft's switches: ${GameSettings.env(root, spellcraft)}")
        check(GameSettings.choices(root, "com.other.game").values.none { it } && GameSettings.env(root, "com.other.game") == "", "another game keeps the bridge's own behaviour")

        GameSettings.set(root, spellcraft, keys.first(), false)
        GameSettings.set(root, "com.other.game", keys.first(), true)
        // A new read from the file, as after a restart.
        check(GameSettings.choices(root, spellcraft)[keys.first()] == false, "a switch turned off stays off")
        check(GameSettings.settings.first().env.split(';').none { it in GameSettings.env(root, spellcraft).split(';') }, "and the game no longer gets it: ${GameSettings.env(root, spellcraft)}")
        check(GameSettings.choices(root, "com.other.game")[keys.first()] == true, "a switch turned on stays on")
        check(GameSettings.choices(root, spellcraft).filterKeys { it != keys.first() }.values.all { it }, "the others keep their defaults")
        check(GameSettings.env(root, "../escape") == "" && !File(root.parentFile, "escape").exists(), "an odd id can't leave the folder")
    } finally {
        root.deleteRecursively()
    }
    println("settings check: ok")
}
