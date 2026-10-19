package dev.braxten.gamebridge

import android.app.Activity
import android.os.Bundle
import android.os.Environment
import android.widget.Toast
import java.io.File

/**
 * Opens an iPhone game for its own app (IphoneApps): the app hands over the game's folder. It is
 * the one part of Game Bridge other apps may open, so it only opens a folder on the phone's
 * shared storage that holds an iPhone game; the game screen itself stays private.
 */
class OpenIphoneGame : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val folder = intent.getStringExtra("app")?.let(::File)?.canonicalFile
        val storage = Environment.getExternalStorageDirectory().canonicalFile
        when {
            folder == null || !folder.path.startsWith(storage.path + "/") || !folder.name.endsWith(".app") -> {}
            !File(folder, "Info.plist").isFile ->
                Toast.makeText(this, "This game's folder isn't on the phone any more.", Toast.LENGTH_LONG).show()
            else -> startActivity(IphoneGames.intent(this, folder))
        }
        finish()
    }
}
