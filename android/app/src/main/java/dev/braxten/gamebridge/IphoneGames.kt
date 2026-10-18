package dev.braxten.gamebridge

import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon
import android.widget.Toast

/** Opening unpacked iPhone games: from their card, and from their own icons. */
object IphoneGames {
    /** The game screen for `game`'s folder. Launchers start shortcuts as Game Bridge, so this works though the screen is private. */
    private fun intent(context: Context, game: Game) = Intent(Intent.ACTION_MAIN)
        .setClass(context, IosGameActivity::class.java)
        .putExtra("app", game.file!!.path)
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

    /** Asks the home screen to add the game's own icon (Android shows its confirmation). */
    fun addToHomeScreen(context: Context, game: Game) {
        if (game.file == null) return
        val shortcuts = context.getSystemService(ShortcutManager::class.java)
        if (shortcuts == null || !shortcuts.isRequestPinShortcutSupported) {
            Toast.makeText(context, "This home screen can't add icons", Toast.LENGTH_SHORT).show()
            return
        }
        publish(context, game)
        shortcuts.requestPinShortcut(shortcut(context, game), null)
    }
}
