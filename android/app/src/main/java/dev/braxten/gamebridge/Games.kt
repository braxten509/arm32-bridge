package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import android.os.Environment
import android.os.storage.StorageManager
import java.io.File
import java.util.zip.ZipFile

/** What the card's button does. */
enum class Action {
    /** 32-bit code: convert it, then install. */
    Convert,
    /** A bundle that already runs on this phone: install its pieces as they are. */
    Install,
}

/**
 * A game: an APK or bundle found on the phone with 32-bit ARM code and no 64-bit code, a bundle
 * that already runs here (Android's own installer can't install bundles), or an already converted
 * game that is installed (then `file` is null when no file of it was found).
 */
class Game(
    val file: File?,
    /** Where it was found, shown under its name. */
    val place: String,
    val label: String,
    val packageName: String,
    val versionCode: Long,
    val icon: Bitmap?,
    val action: Action = Action.Convert,
) {
    /** One game split over several APKs in one file (.xapk, .apkm, .apks). */
    val isBundle get() = file != null && Games.isBundle(file)
}

object Games {
    /** At the top of a drive, "Android" holds other apps' private files, never games to convert. */
    private val skipped = setOf("Android")

    /** Bundle file types: APKPure's .xapk, APKMirror's .apkm, SAI's and bundletool's .apks. */
    private val bundleEndings = listOf(".xapk", ".apkm", ".apks")

    fun isBundle(file: File) = bundleEndings.any { file.name.endsWith(it, ignoreCase = true) }

    /**
     * Every 32-bit-only game APK or bundle in the phone's storage and on SD cards, bundles that
     * already run here, plus converted games that are installed without a file on the phone, by name.
     * Only each APK's file list and manifest are read (for a bundle, its base APK is copied out to read them).
     */
    fun scan(context: Context): List<Game> {
        val found = storageRoots(context).flatMap { (root, name) ->
            apks(root).sortedBy { it.path.lowercase() }.map { it to place(root, name, it) }
        }
        // The same game and version found twice shows once.
        val games = found
            .mapNotNull { (file, place) -> runCatching { read(context, file, place) }.getOrNull() }
            .distinctBy { it.packageName to it.versionCode }
        val withFiles = games.map { it.packageName }.toSet()
        return (games + installed(context).filter { it.packageName !in withFiles }).sortedBy { it.label.lowercase() }
    }

    /** The phone's own storage ("Phone"), then each SD card or USB drive. */
    private fun storageRoots(context: Context): List<Pair<File, String>> {
        val phone = Environment.getExternalStorageDirectory()
        val others = context.getSystemService(StorageManager::class.java).storageVolumes
            .mapNotNull { volume -> volume.directory?.takeIf { it != phone }?.let { it to (volume.getDescription(context) ?: it.name) } }
        return listOf(phone to "Phone") + others
    }

    private fun apks(folder: File): List<File> {
        val entries = folder.listFiles() ?: return emptyList()
        return entries.flatMap { entry ->
            when {
                entry.name.startsWith('.') -> emptyList()
                entry.isDirectory -> if (entry.name in skipped && isRoot(folder)) emptyList() else apks(entry)
                entry.name.endsWith(".apk", ignoreCase = true) || isBundle(entry) -> listOf(entry)
                else -> emptyList()
            }
        }
    }

    private fun isRoot(folder: File) = folder == Environment.getExternalStorageDirectory() ||
        folder.parentFile?.path == "/storage"

    /** "Download", "Documents/Games", or the drive's name for files on an SD card. */
    private fun place(root: File, rootName: String, file: File): String {
        val folder = file.parentFile!!.relativeTo(root).path
        return when {
            rootName == "Phone" && folder.isEmpty() -> "Phone storage"
            rootName == "Phone" -> folder
            folder.isEmpty() -> rootName
            else -> "$rootName/$folder"
        }
    }

    /** Converted games that are installed, shown with their installed name and icon. */
    private fun installed(context: Context): List<Game> {
        val packages = context.packageManager
        return packages.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
            .filter { isConverted(it) && packages.getLaunchIntentForPackage(it.packageName) != null }
            .map { application ->
                val info = packages.getPackageInfo(application.packageName, PackageManager.PackageInfoFlags.of(0))
                Game(null, "Installed", application.loadLabel(packages).toString(), application.packageName,
                    info.longVersionCode, iconBitmap(runCatching { application.loadIcon(packages) }.getOrNull()))
            }
    }

    private fun read(context: Context, file: File, place: String): Game? {
        if (isBundle(file)) return readBundle(context, file, place)
        if (!isThirtyTwoBitOnly(file)) return null
        return fromApk(context, file, file, place, Action.Convert)
    }

    /** The converter looks inside (the pieces' file lists) and copies the base APK out for its name and icon. */
    private fun readBundle(context: Context, file: File, place: String): Game? {
        val work = File(context.cacheDir, "inspect").apply { deleteRecursively(); mkdirs() }
        try {
            val base = File(work, "base.apk")
            val action = when (Converter.inspect(context, file, base, work)["bits"]) {
                "32" -> Action.Convert
                // Already 64-bit, or no native code at all: both run here as they are.
                "64", "none" -> Action.Install
                else -> return null
            }
            return fromApk(context, base, file, place, action)
        } finally {
            work.deleteRecursively()
        }
    }

    /** A card for `file`, with the name, package and icon read from `apk` (the file itself, or a bundle's base). */
    private fun fromApk(context: Context, apk: File, file: File, place: String, action: Action): Game? {
        val packages = context.packageManager
        val info = packages.getPackageArchiveInfo(apk.path, 0) ?: return null
        val application = info.applicationInfo ?: return null
        application.sourceDir = apk.path
        application.publicSourceDir = apk.path
        val icon = iconBitmap(runCatching { application.loadIcon(packages) }.getOrNull())
        return Game(file, place, application.loadLabel(packages).toString(), info.packageName, info.longVersionCode, icon, action)
    }

    private fun iconBitmap(drawable: Drawable?): Bitmap? = drawable?.let {
        val size = 192
        Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888).also { bitmap ->
            it.setBounds(0, 0, size, size)
            it.draw(Canvas(bitmap))
        }
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

    fun isInstalled(context: Context, packageName: String): Boolean = runCatching {
        context.packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
    }.isSuccess

    /** True when a converted copy of this game is installed (its library folder holds a kept 32-bit original). */
    fun isConverted(context: Context, packageName: String): Boolean = runCatching {
        isConverted(context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0)))
    }.getOrDefault(false)

    private fun isConverted(application: ApplicationInfo): Boolean =
        File(application.nativeLibraryDir ?: return false).list()?.any { it.endsWith("-arm32.so") } == true
}
