package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.net.Uri
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.util.zip.ZipFile

/** A game APK (in Downloads or added by the user) that has 32-bit ARM code and no 64-bit code. */
class Game(
    val file: File,
    /** Where it was found, shown under its name. */
    val place: String,
    val label: String,
    val packageName: String,
    val versionCode: Long,
    val icon: Bitmap?,
)

object Games {
    fun downloads(): File = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)

    /** Paths of games the user added with "Add a game", kept between runs. */
    private fun added(context: Context) = context.getSharedPreferences("games", Context.MODE_PRIVATE)

    /** The 32-bit-only games in Downloads and the added ones, by name. Only each APK's file list and manifest are read. */
    fun scan(context: Context): List<Game> {
        val downloaded = downloads().listFiles { file -> file.isFile && file.name.endsWith(".apk", ignoreCase = true) }
            .orEmpty().sortedBy { it.name.lowercase() }.map { it to "Downloads" }
        // An added file that was moved or deleted just stops showing.
        val picked = added(context).getStringSet("paths", emptySet()).orEmpty()
            .map(::File).filter { it.isFile }.sortedBy { it.name.lowercase() }.map { it to "Added by you" }
        // The same game and version found twice shows once.
        return (downloaded + picked)
            .mapNotNull { (file, place) -> runCatching { read(context, file, place) }.getOrNull() }
            .distinctBy { it.packageName to it.versionCode }
            .sortedBy { it.label.lowercase() }
    }

    /**
     * Adds a game the user picked in the phone's file picker. Files on the phone or an SD card are
     * used where they are; files from elsewhere (cloud drives, other apps) are copied into this app first.
     * Returns null when added, or a plain-language reason it wasn't.
     */
    fun add(context: Context, uri: Uri): String? {
        val file = localFile(uri) ?: copyIn(context, uri) ?: return "Couldn't read that file."
        if (!file.name.endsWith(".apk", ignoreCase = true) && !isApk(file)) return "That file isn't an app (APK)."
        if (runCatching { read(context, file, "") }.getOrNull() == null) {
            if (file.startsWith(context.filesDir)) file.delete()
            return "That app has no 32-bit-only game code, so it doesn't need Game Bridge."
        }
        val paths = added(context).getStringSet("paths", emptySet()).orEmpty() + file.path
        added(context).edit().putStringSet("paths", paths).apply()
        return null
    }

    /** The real path of a picked file in shared storage (this app may read all of it). */
    private fun localFile(uri: Uri): File? {
        val id = runCatching { DocumentsContract.getDocumentId(uri) }.getOrNull() ?: return null
        // The Downloads shortcut sometimes names the file directly.
        if (uri.authority == "com.android.providers.downloads.documents" && id.startsWith("raw:")) {
            return File(id.removePrefix("raw:")).takeIf { it.isFile && it.canRead() }
        }
        if (uri.authority != "com.android.externalstorage.documents") return null
        val (volume, path) = id.split(':', limit = 2).takeIf { it.size == 2 } ?: return null
        val root = if (volume == "primary") Environment.getExternalStorageDirectory().path else "/storage/$volume"
        return File(root, path).takeIf { it.isFile && it.canRead() }
    }

    private fun copyIn(context: Context, uri: Uri): File? = runCatching {
        val resolver = context.contentResolver
        val name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }?.replace('/', '_') ?: "game.apk"
        val target = File(File(context.filesDir, "added").apply { mkdirs() }, name)
        resolver.openInputStream(uri)!!.use { input -> target.outputStream().use { input.copyTo(it, 1 shl 20) } }
        target
    }.getOrNull()

    private fun isApk(file: File): Boolean = runCatching { ZipFile(file).use { it.getEntry("AndroidManifest.xml") != null } }.getOrDefault(false)

    private fun read(context: Context, file: File, place: String): Game? {
        if (!isThirtyTwoBitOnly(file)) return null
        val packages = context.packageManager
        val info = packages.getPackageArchiveInfo(file.path, 0) ?: return null
        val application = info.applicationInfo ?: return null
        application.sourceDir = file.path
        application.publicSourceDir = file.path
        val icon = runCatching { application.loadIcon(packages) }.getOrNull()?.let { drawable ->
            val size = 192
            Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888).also { bitmap ->
                drawable.setBounds(0, 0, size, size)
                drawable.draw(Canvas(bitmap))
            }
        }
        return Game(file, place, application.loadLabel(packages).toString(), info.packageName, info.longVersionCode, icon)
    }

    private fun isThirtyTwoBitOnly(file: File): Boolean = ZipFile(file).use { zip ->
        var thirtyTwo = false
        for (entry in zip.entries()) {
            val name = entry.name
            if (!name.startsWith("lib/") || !name.endsWith(".so")) continue
            if (name.startsWith("lib/arm64-v8a/")) return false
            if (name.startsWith("lib/armeabi-v7a/") || name.startsWith("lib/armeabi/")) thirtyTwo = true
        }
        thirtyTwo
    }

    /** True when a converted copy of this game is installed (its library folder holds a kept 32-bit original). */
    fun isConverted(context: Context, packageName: String): Boolean = runCatching {
        val application = context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
        File(application.nativeLibraryDir).list()?.any { it.endsWith("-arm32.so") } == true
    }.getOrDefault(false)
}
