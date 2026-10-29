package dev.braxten.gamebridge

import java.io.File

/**
 * An iPhone game's on/off settings (its card's Settings…): each starts at the game's own
 * default, a choice is kept in a file under the app's files (so it outlasts a restart), and
 * the game gets the iPhone layer's switches of the settings that are on when it starts
 * (IosGameActivity; setprop debug.gamebridge.ios.env still wins, for tests). Animations on
 * the phone are always on (the iPhone layer's default), so they aren't a setting.
 */
object GameSettings {
    /** A setting: its key in the file, what the box shows, and the switches it turns on. */
    class Setting(val key: String, val title: String, val description: String, val env: String, val onFor: Set<String>)

    private const val SPELLCRAFT = "com.appyentertainment.spellcraft"

    val settings = listOf(
        Setting(
            "anim_lead", "Animation timing lead",
            "The game's animation clock runs a little ahead, to the moment its frame reaches the screen.",
            "IOS_ANIM_LEAD=1;IOS_ANIM_LEAD_EXTRA_MS=8", setOf(SPELLCRAFT),
        ),
    )

    /** The file of the game with CFBundleIdentifier `id`; null for an id that isn't a plain name. */
    private fun file(root: File, id: String): File? =
        if (id.isEmpty() || id == "." || id == ".." || id.any { !it.isLetterOrDigit() && it !in "._-" }) null
        else File(File(root, "game-settings"), id)

    private fun saved(root: File, id: String): Map<String, Boolean> =
        file(root, id)?.takeIf { it.isFile }?.readLines().orEmpty()
            .mapNotNull { it.split('=', limit = 2).takeIf { pair -> pair.size == 2 } }
            .associate { (key, value) -> key.trim() to (value.trim() == "1") }

    /** Each setting's key and whether it's on for the game. */
    fun choices(root: File, id: String): Map<String, Boolean> {
        val saved = saved(root, id)
        return settings.associate { it.key to (saved[it.key] ?: (id in it.onFor)) }
    }

    /** Turns a setting on or off for the game. */
    fun set(root: File, id: String, key: String, on: Boolean) {
        val file = file(root, id) ?: return
        val saved = saved(root, id) + (key to on)
        file.parentFile?.mkdirs()
        file.writeText(saved.entries.joinToString("") { "${it.key}=${if (it.value) 1 else 0}\n" })
    }

    /** The switches the game starts with ("NAME=VALUE;..."; empty for none). */
    fun env(root: File, id: String): String {
        val on = choices(root, id)
        return settings.filter { on[it.key] == true }.joinToString(";") { it.env }
    }
}
