package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Environment
import java.io.File
import java.util.zip.ZipFile

/** A game APK in Downloads that has 32-bit ARM code and no 64-bit code. */
class Game(
    val file: File,
    val label: String,
    val packageName: String,
    val versionCode: Long,
    val icon: Bitmap?,
)

object Games {
    fun downloads(): File = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)

    /** The 32-bit-only games in Downloads, by name. Only each APK's file list and manifest are read. */
    fun scan(context: Context): List<Game> {
        val files = downloads().listFiles { file -> file.isFile && file.name.endsWith(".apk", ignoreCase = true) }
            ?: return emptyList()
        // The same game and version downloaded twice shows once.
        return files.sortedBy { it.name.lowercase() }
            .mapNotNull { file -> runCatching { read(context, file) }.getOrNull() }
            .distinctBy { it.packageName to it.versionCode }
            .sortedBy { it.label.lowercase() }
    }

    private fun read(context: Context, file: File): Game? {
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
        return Game(file, application.loadLabel(packages).toString(), info.packageName, info.longVersionCode, icon)
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
