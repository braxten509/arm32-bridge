package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Drawable
import android.os.Environment
import android.os.storage.StorageManager
import java.io.File
import java.io.InputStream
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.zip.ZipFile
import org.json.JSONArray
import org.json.JSONObject

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
    /** An unpacked iPhone game (a folder ending in .app with 64-bit or ARMv7 code): played in Game Bridge's iPhone game screen. */
    PlayIphone,
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
    /** Other files of the same game and version (only one shows as the card); they move to Game Bridge/Originals. */
    val copies: List<File> = emptyList(),
    /** Other data files on the phone that fit it (older versions, a second copy): they move to Game Bridge/Originals. */
    val otherData: List<File> = emptyList(),
) {
    fun copy(file: File? = this.file, place: String = this.place, looseData: DataSource? = this.looseData, copies: List<File> = this.copies) =
        Game(file, place, label, packageName, versionCode, icon, action, dataFile, looseData, copies, otherData)

    /** One game split over several APKs in one file (.xapk, .apkm, .apks). */
    val isBundle get() = file != null && Games.isBundle(file)

    /** The game data file is where the game looks for it, or the game needs none. */
    val dataInPlace get() = dataFile == null || Games.dataInPlace(packageName, versionCode, looseData)
}

/** What a search found: the games, and the spare copies the owner keeps in Game Bridge/Originals (each its own card). */
class Found(val games: List<Game>, val originals: List<Game>)

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
     * Game Bridge/Originals is searched on its own (it is small), for the spare copies' cards.
     */
    fun scan(context: Context): Found {
        val started = System.currentTimeMillis()
        val readBefore = reads
        val roots = storageRoots(context)
        val found = roots.flatMap { (root, name) ->
            files(root).sortedBy { it.path.lowercase() }.map { it to place(root, name, it) }
        }
        // Its spare data files get no cards.
        val spareFiles = roots.flatMap { (root, name) ->
            files(File(File(root, FOLDER), ORIGINALS)).filter { !it.name.endsWith(".obb", ignoreCase = true) && !isZip(it) }
                .sortedBy { it.path.lowercase() }.map { it to place(root, name, it) }
        }
        val originals = spareFiles
            .mapNotNull { (file, place) -> remembered(file) { read(context, file, place) }?.let { named(context, it) } }
            .sortedBy { it.label.lowercase() }
        val (dataFiles, gameFiles) = found.partition { (file, _) -> file.name.endsWith(".obb", ignoreCase = true) || isZip(file) }
        val loose = dataFiles.map { (file, _) -> file }
        // The same game and version found twice shows once: the file already in Game Bridge/Android or
        // /iPhone, else the first one. The others are its copies (Game Bridge/Originals isn't searched).
        val games = gameFiles
            .mapNotNull { (file, place) -> remembered(file) { read(context, file, place) }?.let { withLooseData(named(context, it), loose) } }
            .sortedBy { game -> game.file?.let { if (inPlace(it)) 0 else if (inside(it, ORIGINALS)) 2 else 1 } ?: 1 }
            .groupBy { it.packageName to it.versionCode }.values
            .map { same -> same.first().copy(copies = same.drop(1).mapNotNull { it.file }) }
        val withFiles = games.map { it.packageName }.toSet()
        val installed = installed(context, loose)
        // Files that are gone (deleted, or a game's old APK after an update) are forgotten.
        seen.keys.retainAll(((gameFiles + spareFiles).map { (file, _) -> file.path } + installed.map { it.second }).toSet())
        val list = (games + installed.map { it.first }.filter { it.packageName !in withFiles }).sortedBy { it.label.lowercase() }
        val result = Found(list, originals)
        runCatching { save(context, result) }
        android.util.Log.i("GameBridge", "Search: ${list.size} games, ${originals.size} originals, ${found.size + spareFiles.size} files (${reads - readBefore} read) in ${System.currentTimeMillis() - started} ms")
        return result
    }

    /**
     * The list the last search found, saved on disk, so the app shows it at once while it searches
     * again. Also fills in what was read about each file, so that search reads only new and
     * changed files. Null before the first search finishes.
     */
    fun saved(context: Context): Found? = runCatching {
        val saved = JSONObject(savedFile(context).readText())
        val icons = iconFolder(context)
        // What an older app read is read again: a newer one may read more (icons, names, kinds).
        val files = if (saved.optLong("app") == appInstalled(context)) saved.getJSONArray("seen") else JSONArray()
        for (index in 0 until files.length()) {
            val entry = files.getJSONObject(index)
            val path = entry.getString("path")
            seen.putIfAbsent(path, Seen(entry.getLong("length"), entry.getLong("modified"), entry.optJSONObject("game")?.let { gameFrom(it, icons) }))
        }
        // A file deleted while the app was closed shows no card, not even until the search ends.
        fun list(name: String) = saved.optJSONArray(name)?.let { games ->
            (0 until games.length()).map { named(context, gameFrom(games.getJSONObject(it), icons)) }.filter { it.file?.exists() != false }
        }.orEmpty()
        Found(list("games"), list("originals"))
    }.getOrNull()

    private fun savedFile(context: Context) = File(context.filesDir, "games.json")
    /** When this copy of the app was installed or updated. */
    private fun appInstalled(context: Context) = context.packageManager.getPackageInfo(context.packageName, 0).lastUpdateTime
    private fun iconFolder(context: Context) = File(context.filesDir, "icons")

    /** Keeps `found` and `seen` for [saved]. Each icon is a picture file of its own, written once. */
    private fun save(context: Context, found: Found) {
        val icons = iconFolder(context).apply { mkdirs() }
        val kept = mutableSetOf<String>()
        fun json(game: Game) = jsonOf(game, icons)?.also { (_, icon) -> icon?.let(kept::add) }?.first
        val seenJson = JSONArray()
        for ((path, entry) in seen) {
            seenJson.put(JSONObject().put("path", path).put("length", entry.length).put("modified", entry.modified)
                .put("game", entry.game?.let(::json) ?: JSONObject.NULL))
        }
        val saved = JSONObject().put("games", JSONArray(found.games.mapNotNull(::json))).put("originals", JSONArray(found.originals.mapNotNull(::json)))
            .put("seen", seenJson).put("app", appInstalled(context))
        val file = savedFile(context)
        val part = File(file.path + ".part")
        part.writeText(saved.toString())
        part.renameTo(file)
        icons.listFiles()?.filter { it.name !in kept }?.forEach(File::delete)
    }

    /** `game` for the saved file, with its icon's file name (the icon written if it isn't yet). */
    private fun jsonOf(game: Game, icons: File): Pair<JSONObject, String?>? {
        val icon = game.icon?.let { bitmap ->
            // Named after the picture too: a better reading (a newer app) gets a new file, not the old one.
            val pixels = java.nio.ByteBuffer.allocate(bitmap.byteCount).also(bitmap::copyPixelsToBuffer).array()
            val name = UUID.nameUUIDFromBytes("${game.file?.path ?: game.packageName}:${game.versionCode}:".toByteArray() + pixels).toString() + ".png"
            val file = File(icons, name)
            if (!file.exists()) {
                val part = File(icons, "$name.part")
                part.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
                part.renameTo(file)
            }
            name
        }
        val json = JSONObject()
            .put("file", game.file?.path ?: JSONObject.NULL)
            .put("place", game.place)
            .put("label", game.label)
            .put("package", game.packageName)
            .put("version", game.versionCode)
            .put("icon", icon ?: JSONObject.NULL)
            .put("action", game.action.name)
            .put("data", game.dataFile ?: JSONObject.NULL)
            .put("loose", game.looseData?.let {
                JSONObject().put("file", it.file.path).put("entry", it.entry ?: JSONObject.NULL).put("name", it.name).put("size", it.size)
            } ?: JSONObject.NULL)
            .put("copies", JSONArray(game.copies.map { it.path }))
            .put("otherData", JSONArray(game.otherData.map { it.path }))
        return json to icon
    }

    private fun files(paths: JSONArray?) = paths?.let { (0 until it.length()).map { index -> File(it.getString(index)) } }.orEmpty()

    private fun gameFrom(json: JSONObject, icons: File): Game {
        fun text(name: String) = if (json.isNull(name)) null else json.getString(name)
        val loose = json.optJSONObject("loose")?.let {
            DataSource(File(it.getString("file")), if (it.isNull("entry")) null else it.getString("entry"), it.getString("name"), it.getLong("size"))
        }
        return Game(
            text("file")?.let(::File), json.getString("place"), json.getString("label"), json.getString("package"), json.getLong("version"),
            text("icon")?.let { BitmapFactory.decodeFile(File(icons, it).path) }, Action.valueOf(json.getString("action")), text("data"), loose,
            files(json.optJSONArray("copies")), files(json.optJSONArray("otherData")),
        )
    }

    /** What reading a file found, kept while the app runs. */
    private class Seen(val length: Long, val modified: Long, val game: Game?)

    /** By path. Reading every file takes a while, so searching again only reads new and changed ones. */
    private val seen = ConcurrentHashMap<String, Seen>()

    /** Files read so far (not answered from `seen`), for the search's line in the log. */
    @Volatile private var reads = 0

    /** `read`'s answer for `file`, read again only when the file changed. A failed read is tried again next time. */
    private fun remembered(file: File, read: () -> Game?): Game? {
        val (length, modified) = file.length() to file.lastModified()
        seen[file.path]?.takeIf { it.length == length && it.modified == modified }?.let { return it.game }
        reads++
        val game = try {
            read()
        } catch (unsure: Unsure) {
            return unsure.game
        } catch (_: Exception) {
            return null
        }
        seen[file.path] = Seen(length, modified, game)
        return game
    }

    /** A card read only in part (whether it needs a data file couldn't be checked): shown, not remembered. */
    private class Unsure(val game: Game) : Exception()

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
                inspect(context, apk, File(work, "unused.apk"), work)["download"] == "yes"
            } finally {
                work.deleteRecursively()
            }
        }
        // Play's name for a game's main data file.
        return if (starts) "main.$versionCode.$packageName.obb" else null
    }

    /** `game` with the data file it needs, read from `apk` (its APK or a bundle's base). */
    private fun needingData(context: Context, game: Game, apk: File, startsDownload: Boolean? = null): Game {
        val data = runCatching { dataFile(context, apk, game.packageName, game.versionCode, startsDownload) }
            .getOrElse { throw Unsure(game) } ?: return game
        return Game(game.file, game.place, game.label, game.packageName, game.versionCode, game.icon, game.action, data)
    }

    /** `game` with a copy of its data file found on the phone, if any: a loose .obb file first, else one inside a .zip. */
    private fun withLooseData(game: Game, found: List<File>): Game {
        val data = game.dataFile ?: return game
        val (zips, loose) = found.partition(::isZip)
        // A known file of the wrong size is damaged or another one: as good as missing.
        fun usable(source: DataSource) = fits(source.name, game) && knownData[source.name]?.let { it.size == source.size } ?: true
        val looseSources = loose.map { DataSource(it, null, it.name, it.length()) }.filter(::usable)
        val zipSources = zips.flatMap { zipped(it, game) }.filter(::usable)
        // Of the newest, one already in the Game Bridge folder, so moving settles on it.
        val best = looseSources.ifEmpty { zipSources }
            .maxWithOrNull(compareBy({ dataVersion(it.name, game.packageName)!! }, { inside(it.file, ANDROID) }))
        val others = (looseSources + zipSources).map { it.file }.distinct().filter { it != best?.file }
        return Game(game.file, game.place, game.label, game.packageName, game.versionCode, game.icon, game.action, data, best, otherData = others)
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
                // An unpacked iPhone game: the folder is the game.
                entry.isDirectory && entry.name.endsWith(".app", ignoreCase = true) && File(entry, "Info.plist").isFile -> listOf(entry)
                entry.isDirectory -> if (entry.name in skipped && isRoot(folder) || isOriginals(entry)) emptyList() else files(entry)
                entry.name.endsWith(".apk", ignoreCase = true) || isBundle(entry) -> listOf(entry)
                entry.name.endsWith(".obb", ignoreCase = true) || isZip(entry) -> listOf(entry)
                else -> emptyList()
            }
        }
    }

    /** Game Bridge/Originals holds spare copies the owner keeps: left out of the search for games ([scan] reads it on its own). */
    private fun isOriginals(folder: File) = folder.name == ORIGINALS && folder.parentFile?.let { it.name == FOLDER && isRoot(it.parentFile ?: it) } == true

    private fun isRoot(folder: File) = folder == Environment.getExternalStorageDirectory() ||
        folder.parentFile?.path == "/storage"

    /**
     * Every game lives in one folder at the top of its drive, so backing up that folder backs up
     * every game: APKs, bundles and their data files in Game Bridge/Android, iPhone games in
     * Game Bridge/iPhone, other copies of a game in Game Bridge/Originals. A file stays on its
     * drive: moving it there is instant, and moving between drives would mean copying.
     */
    const val FOLDER = "Game Bridge"
    private const val ANDROID = "Android"
    private const val IPHONE = "iPhone"
    private const val ORIGINALS = "Originals"

    /** The top of the drive `file` is on (phone storage, an SD card), or null. */
    private fun driveOf(file: File): File? = generateSequence(file.absoluteFile.parentFile) { it.parentFile }.firstOrNull(::isRoot)

    private fun folderFor(file: File, kind: String) = driveOf(file)?.let { File(File(it, FOLDER), kind) }

    /** `file` is somewhere in Game Bridge/`kind` (inside folders of the owner's own too). */
    private fun inside(file: File, kind: String) = folderFor(file, kind)?.let { file.path.startsWith(it.path + "/") } == true

    private fun isIphone(file: File) = file.isDirectory && file.name.endsWith(".app", ignoreCase = true)

    /** A card's file is where it belongs. */
    private fun inPlace(file: File) = inside(file, if (isIphone(file)) IPHONE else ANDROID)

    /**
     * Each of the game's files that isn't in the Game Bridge folder yet, with the folder it goes to.
     * A file no longer there has moved (or gone), so a card stops asking as soon as it moves.
     */
    private fun moves(game: Game): List<Pair<File, File>> = buildList {
        fun add(file: File, kind: String) {
            if (!inside(file, kind) && file.exists()) folderFor(file, kind)?.let { add(file to it) }
        }
        game.file?.let { add(it, if (isIphone(it)) IPHONE else ANDROID) }
        game.looseData?.let { add(it.file, ANDROID) }
        game.copies.forEach { add(it, ORIGINALS) }
        // Spare data files anywhere in the Game Bridge folder stay put.
        game.otherData.filter { !inside(it, ANDROID) }.forEach { add(it, ORIGINALS) }
    }

    /** The card shows Move instead of its other buttons until this is false. */
    fun needsMove(game: Game) = moves(game).isNotEmpty()

    /**
     * Moves the game's files into the Game Bridge folder; a problem to show, or null. Each file keeps
     * its name (a game data file is found by its name, an iPhone game's saves go by its folder's), so
     * a name already taken there goes in a numbered folder inside it ("2", "3", …).
     */
    fun move(context: Context, game: Game): String? = move(context, moves(game))

    /** The card's file alone, with the folder of `kind` on its drive it goes to. */
    private fun alone(game: Game, kind: (File) -> String): List<Pair<File, File>> =
        listOfNotNull(game.file?.takeIf { it.exists() }?.let { file -> folderFor(file, kind(file))?.let { file to it } })

    /** Moves the card's file (only it, not its data file or copies) into Game Bridge/Originals, as a spare copy. */
    fun moveToOriginals(context: Context, game: Game): String? = move(context, alone(game) { ORIGINALS })

    /** Moves a spare copy out of Game Bridge/Originals to Game Bridge/Android or /iPhone, where it is a game's card again. */
    fun moveBack(context: Context, game: Game): String? = move(context, alone(game) { if (isIphone(it)) IPHONE else ANDROID })

    /** Moves each file into its folder, with the rules above. */
    private fun move(context: Context, moves: List<Pair<File, File>>): String? {
        val problems = mutableListOf<String>()
        for ((file, folder) in moves) {
            // Moved already (a data file two versions share).
            if (!file.exists()) continue
            val target = generateSequence(1) { it + 1 }.map { File(if (it == 1) folder else File(folder, "$it"), file.name) }.first { !it.exists() }
            target.parentFile!!.mkdirs()
            if (!file.renameTo(target)) {
                problems += "${file.name} couldn't be moved to ${target.parentFile!!.relativeTo(driveOf(target)!!).path}."
                continue
            }
            if (isIphone(target)) remember(context, file, target)
            // What was read about it still holds (moving keeps its size and date), so it isn't read again.
            seen.remove(file.path)?.let { entry ->
                val place = storageRoots(context).firstOrNull { (root, _) -> target.path.startsWith(root.path + "/") }
                    ?.let { (root, name) -> place(root, name, target) } ?: entry.game?.place.orEmpty()
                seen[target.path] = Seen(entry.length, entry.modified, entry.game?.copy(file = target, place = place))
            }
        }
        return problems.firstOrNull()
    }

    private const val MOVED = "moved-folders"

    /** An iPhone game's old folder → its new one, for home-screen apps and shortcuts that still have the old one. */
    private fun remember(context: Context, from: File, to: File) {
        context.getSharedPreferences(MOVED, Context.MODE_PRIVATE).edit().putString(from.path, to.path).commit()
    }

    /** Where an iPhone game folder is now: `folder` itself, or where Game Bridge moved it. */
    fun current(context: Context, folder: File): File {
        val moved = context.getSharedPreferences(MOVED, Context.MODE_PRIVATE)
        // Moved more than once: follow each step (a few at most, so a loop of entries can't hang).
        return generateSequence(folder) { from -> if (File(from, "Info.plist").isFile) null
            else (moved.getString(from.path, null) ?: moved.getString(from.canonicalPath, null))?.let(::File) }
            .take(8).last()
    }

    /** Where iPhone games go on the phone's own storage. */
    fun iphoneFolder() = File(File(Environment.getExternalStorageDirectory(), FOLDER), IPHONE)

    /** Where Android games go on the phone's own storage. */
    fun androidFolder() = File(File(Environment.getExternalStorageDirectory(), FOLDER), ANDROID)

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

    /** Converted games that are installed, shown with their installed name and icon, each with its APK's path. */
    private fun installed(context: Context, loose: List<File>): List<Pair<Game, String>> {
        val packages = context.packageManager
        return packages.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
            // Game Bridge carries the frame helper itself, so it would look converted.
            .filter { it.packageName != context.packageName }
            .filter { (isConverted(it) || hasPacer(context, it.packageName)) && packages.getLaunchIntentForPackage(it.packageName) != null }
            .mapNotNull { application ->
                val apk = File(application.sourceDir)
                val game = remembered(apk) {
                    val info = packages.getPackageInfo(application.packageName, PackageManager.PackageInfoFlags.of(0))
                    val game = Game(null, "Installed", application.loadLabel(packages).toString(), application.packageName,
                        info.longVersionCode, iconBitmap(runCatching { application.loadIcon(packages) }.getOrNull()))
                    needingData(context, game, apk)
                } ?: return@mapNotNull null
                withLooseData(game, loose) to apk.path
            }
    }

    /** The card for a game file, with the data file it needs (not yet looked for on the phone). */
    private fun read(context: Context, file: File, place: String): Game? {
        if (file.isDirectory) return readIphone(context, file, place)
        if (isBundle(file)) return readBundle(context, file, place)
        val bits = nativeBits(file) ?: return null
        // Plenty of ordinary apps are 64-bit too; only games are listed.
        if (bits == 64 && !isGame(context, file) && !builtWithEngine(context, file)) return null
        val game = fromApk(context, file, file, place, Action.Convert) ?: return null
        return needingData(context, game, file)
    }

    /**
     * The game with the name the user gave it (Rename), if any. Applied after `remembered`, which
     * keeps the name the game itself has, so a rename shows at once and an undone one goes back
     * at the next search.
     */
    private fun named(context: Context, game: Game): Game {
        if (game.action != Action.PlayIphone) return game
        val label = IphoneGames.customName(context, game.packageName) ?: return game
        if (label == game.label) return game
        return Game(game.file, game.place, label, game.packageName, game.versionCode, game.icon, game.action, game.dataFile, game.looseData)
    }

    /** An unpacked iPhone game: its name, id and icon from its Info.plist; null without ARM code it can run. */
    private fun readIphone(context: Context, folder: File, place: String): Game? {
        val info = Plist.read(File(folder, "Info.plist")) as? Map<*, *> ?: return null
        fun text(key: String) = (info[key] as? String)?.takeIf { it.isNotBlank() }
        val program = File(folder, text("CFBundleExecutable") ?: return null)
        if (!hasPlayableCode(program)) return null
        val id = text("CFBundleIdentifier") ?: folder.name
        val label = text("CFBundleDisplayName") ?: text("CFBundleName") ?: folder.nameWithoutExtension
        return Game(folder, place, label, id, 0, iphoneIcon(folder, info), Action.PlayIphone)
    }

    /**
     * Code the iPhone layer runs in an Apple program (a thin one, or one part of a universal file):
     * CPU_TYPE_ARM64, or CPU_TYPE_ARM as ARMv7 / ARMv7s (32-bit, run on the translator).
     */
    private fun hasPlayableCode(program: File): Boolean = runCatching {
        val head = program.inputStream().use { it.readNBytes(4096) }
        val buffer = java.nio.ByteBuffer.wrap(head)
        fun playable(cpu: Int, subtype: Int) = cpu == 0x0100000C || (cpu == 0x0000000C && (subtype and 0x00FFFFFF) in listOf(9, 11))
        when (buffer.getInt(0)) {
            0xCAFEBABE.toInt() -> (0 until minOf(buffer.getInt(4), (head.size - 8) / 20)).any { playable(buffer.getInt(8 + 20 * it), buffer.getInt(12 + 20 * it)) }
            0xCFFAEDFE.toInt(), 0xCEFAEDFE.toInt() -> buffer.order(java.nio.ByteOrder.LITTLE_ENDIAN).let { playable(it.getInt(4), it.getInt(8)) }
            else -> false
        }
    }.getOrDefault(false)

    /** The largest icon the game lists (iPhone and iPad lists, with Apple's @2x/@3x names), or any Icon*.png. */
    private fun iphoneIcon(folder: File, info: Map<*, *>): Bitmap? {
        val names = mutableListOf<String>()
        fun add(value: Any?) {
            when (value) {
                is String -> names.add(value)
                is List<*> -> value.forEach(::add)
                is Map<*, *> -> { add(value["CFBundleIconFiles"]); add(value["CFBundlePrimaryIcon"]) }
            }
        }
        add(info["CFBundleIconFiles"]); add(info["CFBundleIconFile"]); add(info["CFBundleIcons"]); add(info["CFBundleIcons~ipad"])
        val listed = names.flatMap { name ->
            val base = name.removeSuffix(".png")
            listOf("$base.png", "$base@2x.png", "$base@3x.png", "$base@2x~ipad.png", "$base~ipad.png")
        }.map { File(folder, it) }.filter { it.isFile }
        val files = listed.ifEmpty { folder.listFiles().orEmpty().filter { it.name.startsWith("Icon", true) && it.name.endsWith(".png", true) } }
        // Most games ship Apple's "crushed" PNGs, which Android can't read: ApplePng turns them back into normal ones.
        fun png(file: File): ByteArray = file.readBytes().let { ApplePng.uncrush(it) ?: it }
        // Width from the PNG header; a crushed one has its 16-byte CgBI chunk before the header.
        fun width(file: File) = runCatching {
            val head = java.nio.ByteBuffer.wrap(file.inputStream().use { it.readNBytes(36) })
            if (head.getInt(12) == 0x43674249) head.getInt(32) else head.getInt(16)
        }.getOrDefault(-1)
        return files.sortedByDescending(::width).firstNotNullOfOrNull { file -> png(file).let { BitmapFactory.decodeByteArray(it, 0, it.size) } }
    }

    /** The converter looks inside (the pieces' file lists) and copies the base APK out for its name and icon. */
    private fun readBundle(context: Context, file: File, place: String): Game? {
        val work = File(context.cacheDir, "inspect").apply { deleteRecursively(); mkdirs() }
        try {
            val base = File(work, "base.apk")
            val facts = inspect(context, file, base, work)
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
            return if (facts["data"] == "yes") game else needingData(context, game, base, facts["download"] == "yes")
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
    fun iconBitmap(drawable: Drawable?): Bitmap? = drawable?.let {
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

    /** The converter's facts about a file; an empty answer means it failed to read it. */
    private fun inspect(context: Context, input: File, base: File, work: File): Map<String, String> =
        Converter.inspect(context, input, base, work).ifEmpty { throw java.io.IOException("the converter couldn't read ${input.name}") }

    /** Whether an APK is built with a game engine, a known one or its own (the converter's `engine` answer). */
    private fun builtWithEngine(context: Context, apk: File): Boolean {
        val work = File(context.cacheDir, "inspect-engine").apply { deleteRecursively(); mkdirs() }
        try {
            return inspect(context, apk, File(work, "unused.apk"), work)["engine"] == "yes"
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
