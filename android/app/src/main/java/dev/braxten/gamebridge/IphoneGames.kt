package dev.braxten.gamebridge

import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon

/** Opening unpacked iPhone games: from their card, and from their own icons. */
object IphoneGames {
    private const val NAMES = "iphone-names"

    /** The name the user gave a game (by its CFBundleIdentifier), or null for the game's own. */
    fun customName(context: Context, id: String): String? =
        context.getSharedPreferences(NAMES, Context.MODE_PRIVATE).getString(id, null)?.takeIf { it.isNotBlank() }

    /** Renames a game; a blank name goes back to the game's own. */
    fun rename(context: Context, id: String, name: String) {
        val names = context.getSharedPreferences(NAMES, Context.MODE_PRIVATE).edit()
        if (name.isBlank()) names.remove(id) else names.putString(id, name.trim())
        names.commit()
    }

    /** The game screen for `game`'s folder. Launchers start shortcuts as Game Bridge, so this works though the screen is private. */
    private fun intent(context: Context, game: Game) = intent(context, game.file!!)

    /** The game screen for an iPhone game folder. */
    fun intent(context: Context, folder: java.io.File): Intent = Intent(Intent.ACTION_MAIN)
        .setClass(context, IosGameActivity::class.java)
        .putExtra("app", folder.path)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    fun play(context: Context, game: Game) {
        if (game.file == null) return
        context.startActivity(intent(context, game))
        publish(context, game)
    }

    private fun shortcut(context: Context, game: Game): ShortcutInfo {
        val icon = game.icon?.let(Icon::createWithBitmap) ?: Icon.createWithResource(context, R.mipmap.ic_launcher)
        return ShortcutInfo.Builder(context, "iphone:${game.packageName}")
            .setShortLabel(game.label)
            .setIcon(icon)
            .setIntent(intent(context, game))
            .build()
    }

    /** The game shows in Game Bridge's own icon menu (press and hold it), most recent first. */
    private fun publish(context: Context, game: Game) {
        val shortcuts = context.getSystemService(ShortcutManager::class.java) ?: return
        runCatching { shortcuts.pushDynamicShortcut(shortcut(context, game)) }
    }
}
