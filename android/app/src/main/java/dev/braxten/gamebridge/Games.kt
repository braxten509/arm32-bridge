package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Drawable
import android.os.Environment
import android.os.storage.StorageManager
import java.io.File
import java.io.InputStream
import java.util.zip.ZipFile

/** A game data file found on the phone: a loose .obb file, or one inside a .zip (`entry`). */
class DataSource(val file: File, val entry: String?, val name: String, val size: Long) {
    /** "Download" or "Download/Pirates.zip". */
    val where get() = (file.parentFile?.name ?: "") + (if (entry != null) "/${file.name}" else "")

    fun <T> read(use: (InputStream) -> T): T =
        if (entry == null) file.inputStream().use(use)
        else ZipFile(file).use { zip -> zip.getInputStream(zip.getEntry(entry)).use(use) }
}

/** What the card's button does. */
enum class Action {
    /** 32-bit code: convert it, then install. 64-bit game: add the frame helper, then install. */
    Convert,
    /** A bundle that already runs on this phone: install its pieces as they are. */
    Install,
}

/**
 * A game: an APK or bundle found on the phone with 32-bit ARM code (converted to run through the
 * bridge) or a 64-bit game (converted to get the frame helper, which lifts its frame caps), a bundle
 * that already runs here and isn't a game (Android's own installer can't install bundles), or an
 * already converted game that is installed (then `file` is null when no file of it was found).
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
    /** The game data file the game needs in Android/obb (main.<version>.<package>.obb), or null when it needs none. */
    val dataFile: String? = null,
    /** Its data file (or an older version's, which it may use) found on the phone, to copy into place on install. */
    val looseData: DataSource? = null,
) {
    /** One game split over several APKs in one file (.xapk, .apkm, .apks). */
    val isBundle get() = file != null && Games.isBundle(file)

    /** The game data file is where the game looks for it, or the game needs none. */
    val dataInPlace get() = dataFile == null || Games.dataInPlace(packageName, versionCode, looseData)
}

object Games {
    /** At the top of a drive, "Android" holds other apps' private files, never games to convert. */
    private val skipped = setOf("Android")

    /** Bundle file types: APKPure's .xapk, APKMirror's .apkm, SAI's and bundletool's .apks. */
    private val bundleEndings = listOf(".xapk", ".apkm", ".apks")

    fun isBundle(file: File) = bundleEndings.any { file.name.endsWith(it, ignoreCase = true) }

    /**
     * Every game APK or bundle with ARM code in the phone's storage and on SD cards (32-bit and
     * 64-bit), other bundles that run here as they are, plus converted games that are installed
     * without a file on the phone, by name.
     * Only each APK's file list and manifest are read (for a bundle, its base APK is copied out to read them).
     */
    fun scan(context: Context): List<Game> {
        val found = storageRoots(context).flatMap { (root, name) ->
            files(root).sortedBy { it.path.lowercase() }.map { it to place(root, name, it) }
        }
        val (dataFiles, gameFiles) = found.partition { (file, _) -> file.name.endsWith(".obb", ignoreCase = true) || isZip(file) }
        val loose = dataFiles.map { (file, _) -> file }
        // The same game and version found twice shows once.
        val games = gameFiles
            .mapNotNull { (file, place) -> runCatching { read(context, file, place, loose) }.getOrNull() }
            .distinctBy { it.packageName to it.versionCode }
        val withFiles = games.map { it.packageName }.toSet()
        return (games + installed(context, loose).filter { it.packageName !in withFiles }).sortedBy { it.label.lowercase() }
    }

    private fun isZip(file: File) = file.name.endsWith(".zip", ignoreCase = true)

    /** Data files whose right copy is known: a damaged or different file is turned down when copying. */
    class KnownData(val size: Long, val sha256: String)
    val knownData = mapOf(
        // Assassin's Creed Pirates 2.9.1.
        "main.253.com.ubisoft.assassin.pirates.obb" to KnownData(817_249_909, "0bd4bb085dd8b6fbd37d31c9adb55a4ad891f9a91ee49454759497127f6cd704"),
    )

    /** Where Android keeps a game's data files; Android deletes the folder when the game is uninstalled. */
    fun obbFolder(packageName: String) = File(Environment.getExternalStorageDirectory(), "Android/obb/$packageName")

    /**
     * Games made with Google Play's expansion downloader keep most of their data in a separate
     * file the Play Store used to download next to them. It no longer does for these old games, so
     * the file has to come with the game. An APK over Play's 100 MB limit carries its data itself
     * (the converter skips its download screen), and a game whose own code never starts the
     * download only carries the kit unused (the converter's `download` answer).
     */
    private const val EXPANSION_DOWNLOADER = "Lcom/google/android/vending/expansion/downloader/"
    private const val PLAY_EXPANSION_APK_LIMIT = 100L * 1024 * 1024

    private fun dataFile(context: Context, apk: File, packageName: String, versionCode: Long, startsDownload: Boolean?): String? {
        val hasKit = ZipFile(apk).use { zip ->
            val entries = zip.entries().toList()
            // Measured without native libraries, so a converted copy (which adds the bridge) measures the same.
            if (entries.filter { !it.name.startsWith("lib/") }.sumOf { it.compressedSize.coerceAtLeast(0) } > PLAY_EXPANSION_APK_LIMIT) return null
            entries.filter { Regex("classes\\d*\\.dex").matches(it.name) }.any { entry ->
                zip.getInputStream(entry).use { String(it.readBytes(), Charsets.ISO_8859_1).contains(EXPANSION_DOWNLOADER) }
            }
        }
        if (!hasKit) return null
        val starts = startsDownload ?: run {
            val work = File(context.cacheDir, "inspect-data").apply { deleteRecursively(); mkdirs() }
            try {
                Converter.inspect(context, apk, File(work, "unused.apk"), work)["download"] == "yes"
            } finally {
                work.deleteRecursively()
            }
        }
        // Play's name for a game's main data file.
        return if (starts) "main.$versionCode.$packageName.obb" else null
    }

    /**
     * `game` with the data file it needs (read from `apk`, its APK or a bundle's base) and a copy of
     * it found on the phone, if any: a loose .obb file first, else one inside a .zip.
     */
    private fun withData(context: Context, game: Game, apk: File, found: List<File>, startsDownload: Boolean? = null): Game {
        val data = runCatching { dataFile(context, apk, game.packageName, game.versionCode, startsDownload) }.getOrNull() ?: return game
        val (zips, loose) = found.partition(::isZip)
        // A known file of the wrong size is damaged or another one: as good as missing.
        fun usable(source: DataSource) = fits(source.name, game) && knownData[source.name]?.let { it.size == source.size } ?: true
        val sources = loose.map { DataSource(it, null, it.name, it.length()) }.filter(::usable)
            .ifEmpty { zips.flatMap { zipped(it, game) }.filter(::usable) }
        val best = sources.maxByOrNull { dataVersion(it.name, game.packageName)!! }
        return Game(game.file, game.place, game.label, game.packageName, game.versionCode, game.icon, game.action, data, best)
    }

    /** The game's data files inside `zip` (only its file list is read). */
    private fun zipped(zip: File, game: Game): List<DataSource> = runCatching {
        ZipFile(zip).use { archive ->
            archive.entries().toList().filter { !it.isDirectory }.mapNotNull { entry ->
                val name = entry.name.substringAfterLast('/')
                DataSource(zip, entry.name, name, entry.size).takeIf { fits(name, game) }
            }
        }
    }.getOrDefault(emptyList())

    private fun fits(name: String, game: Game) = fits(name, game.packageName, game.versionCode)

    /** The version in a main data file's name (main.<version>.<package>.obb), when it is this game's. */
    private fun dataVersion(name: String, packageName: String): Long? =
        name.takeIf { it.startsWith("main.") && it.endsWith(".$packageName.obb") }
            ?.removePrefix("main.")?.removeSuffix(".$packageName.obb")?.toLongOrNull()

    /** A new version of a game may keep using an older version's data file, so any version up to its own fits. */
    private fun fits(name: String, packageName: String, versionCode: Long) =
        dataVersion(name, packageName)?.let { it <= versionCode } == true

    /**
     * A data file that fits is where the game looks for it. When a loose copy was found, the one in
     * place must be that file at its full size (an earlier copy may have stopped partway).
     */
    fun dataInPlace(packageName: String, versionCode: Long, loose: DataSource?): Boolean {
        val placed = obbFolder(packageName).listFiles().orEmpty()
            .filter { it.isFile && it.length() > 0 && fits(it.name, packageName, versionCode) }
        return if (loose != null) placed.any { it.name == loose.name && it.length() == loose.size } else placed.isNotEmpty()
    }

    /** The phone's own storage ("Phone"), then each SD card or USB drive. */
    private fun storageRoots(context: Context): List<Pair<File, String>> {
        val phone = Environment.getExternalStorageDirectory()
        val others = context.getSystemService(StorageManager::class.java).storageVolumes
            .mapNotNull { volume -> volume.directory?.takeIf { it != phone }?.let { it to (volume.getDescription(context) ?: it.name) } }
        return listOf(phone to "Phone") + others
    }

    /** Game files (APKs, bundles), loose game data files (.obb) and .zip files (which may hold one). */
    private fun files(folder: File): List<File> {
        val entries = folder.listFiles() ?: return emptyList()
        return entries.flatMap { entry ->
            when {
                entry.name.startsWith('.') -> emptyList()
                entry.isDirectory -> if (entry.name in skipped && isRoot(folder)) emptyList() else files(entry)
                entry.name.endsWith(".apk", ignoreCase = true) || isBundle(entry) -> listOf(entry)
                entry.name.endsWith(".obb", ignoreCase = true) || isZip(entry) -> listOf(entry)
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
    private fun installed(context: Context, loose: List<File>): List<Game> {
        val packages = context.packageManager
        return packages.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
            // Game Bridge carries the frame helper itself, so it would look converted.
            .filter { it.packageName != context.packageName }
            .filter { (isConverted(it) || hasPacer(context, it.packageName)) && packages.getLaunchIntentForPackage(it.packageName) != null }
            .map { application ->
                val info = packages.getPackageInfo(application.packageName, PackageManager.PackageInfoFlags.of(0))
                val game = Game(null, "Installed", application.loadLabel(packages).toString(), application.packageName,
                    info.longVersionCode, iconBitmap(runCatching { application.loadIcon(packages) }.getOrNull()))
                withData(context, game, File(application.sourceDir), loose)
            }
    }

    private fun read(context: Context, file: File, place: String, loose: List<File>): Game? {
        if (isBundle(file)) return readBundle(context, file, place, loose)
        val bits = nativeBits(file) ?: return null
        // Plenty of ordinary apps are 64-bit too; only games are listed.
        if (bits == 64 && !isGame(context, file) && !builtWithEngine(context, file)) return null
        val game = fromApk(context, file, file, place, Action.Convert) ?: return null
        return withData(context, game, file, loose)
    }

    /** The converter looks inside (the pieces' file lists) and copies the base APK out for its name and icon. */
    private fun readBundle(context: Context, file: File, place: String, loose: List<File>): Game? {
        val work = File(context.cacheDir, "inspect").apply { deleteRecursively(); mkdirs() }
        try {
            val base = File(work, "base.apk")
            val facts = Converter.inspect(context, file, base, work)
            val action = when (facts["bits"]) {
                "32" -> Action.Convert
                // A 64-bit game gets the frame helper; other 64-bit bundles, and those without
                // native code, run here as they are.
                "64" -> if (isGame(context, base) || facts["engine"] == "yes") Action.Convert else Action.Install
                "none" -> Action.Install
                else -> return null
            }
            val game = fromApk(context, base, file, place, action) ?: return null
            // A bundle usually carries its data file; one that doesn't needs it like a lone APK.
            return if (facts["data"] == "yes") game else withData(context, game, base, loose, facts["download"] == "yes")
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

    /**
     * The icon as a square picture. An adaptive icon (converted games get one, see the converter's
     * icon.rs) is drawn without the phone's shape mask: its layers are 1.5 times the icon, so the
     * middle two thirds show, which for a converted game is exactly its original picture.
     */
    private fun iconBitmap(drawable: Drawable?): Bitmap? = drawable?.let {
        val size = 192
        Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888).also { bitmap ->
            val canvas = Canvas(bitmap)
            if (it is AdaptiveIconDrawable) {
                val margin = size / 4
                listOfNotNull(it.background, it.foreground).forEach { layer ->
                    layer.setBounds(-margin, -margin, size + margin, size + margin)
                    layer.draw(canvas)
                }
            } else {
                it.setBounds(0, 0, size, size)
                it.draw(canvas)
            }
        }
    }

    /** 64 for any 64-bit ARM library, 32 for 32-bit ARM ones only, null for no ARM code. */
    private fun nativeBits(file: File): Int? = ZipFile(file).use { zip ->
        var thirtyTwo = false
        for (entry in zip.entries()) {
            val name = entry.name
            if (!name.startsWith("lib/") || !name.endsWith(".so")) continue
            if (name.startsWith("lib/arm64-v8a/")) return 64
            if (name.startsWith("lib/armeabi-v7a/") || name.startsWith("lib/armeabi/")) thirtyTwo = true
        }
        if (thirtyTwo) 32 else null
    }

    /** Whether an app tells Android it is a game (its category, or the older isGame flag in `apk`'s manifest). */
    private fun isGame(context: Context, apk: File): Boolean {
        val application = context.packageManager.getPackageArchiveInfo(apk.path, 0)?.applicationInfo ?: return false
        @Suppress("DEPRECATION")
        return application.category == ApplicationInfo.CATEGORY_GAME || application.flags and ApplicationInfo.FLAG_IS_GAME != 0
    }

    /** Whether an APK is built with a known game engine (the converter's `engine` answer). */
    private fun builtWithEngine(context: Context, apk: File): Boolean {
        val work = File(context.cacheDir, "inspect-engine").apply { deleteRecursively(); mkdirs() }
        try {
            return Converter.inspect(context, apk, File(work, "unused.apk"), work)["engine"] == "yes"
        } finally {
            work.deleteRecursively()
        }
    }

    fun isInstalled(context: Context, packageName: String): Boolean = runCatching {
        context.packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
    }.isSuccess

    /**
     * True when a converted copy of this game is installed: its library folder holds a kept 32-bit
     * original, or the 64-bit frame helper.
     */
    fun isConverted(context: Context, packageName: String): Boolean = runCatching {
        isConverted(context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))) ||
            hasPacer(context, packageName)
    }.getOrDefault(false)

    /** The frame helper's loader is listed in the installed game (also when its libraries stay inside the APK). */
    private fun hasPacer(context: Context, packageName: String): Boolean = runCatching {
        context.packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_PROVIDERS.toLong()))
            .providers.orEmpty().any { it.name == PACER_CLASS }
    }.getOrDefault(false)

    private fun isConverted(application: ApplicationInfo): Boolean =
        File(application.nativeLibraryDir ?: return false).list()?.any { it.endsWith("-arm32.so") || it == PACER_FILE } == true

    /** The frame helper's file name in a converted 64-bit game, and the class that loads it. */
    const val PACER_FILE = "libgamebridge_pacer.so"
    private const val PACER_CLASS = "dev.braxten.gamebridge.Pacer"
}
