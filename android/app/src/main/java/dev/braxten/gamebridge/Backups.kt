package dev.braxten.gamebridge

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Environment
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.security.MessageDigest

/** A game found in the backup folder. */
class SavedGame(
    val packageName: String,
    val label: String,
    val versionCode: Long,
    val versionName: String,
    val icon: Bitmap?,
    /** Bytes of everything saved for it (its APKs, data files and original files). */
    val size: Long,
    /** Its folder in the backup (a document in the picked folder's tree). */
    val folder: Uri,
    /** Its game data files (name to size). */
    val dataFiles: Map<String, Long>,
    /** For an iPhone game, when its saves were backed up (milliseconds); null for an Android game. */
    val savesFrom: Long? = null,
    /** For an iPhone game, its home folder name (files/ios/<name>). */
    val homeName: String? = null,
    /** For an iPhone game, where its game folder was when backed up. */
    val place: String? = null,
) {
    val isIphone get() = savesFrom != null

    /** This version is installed, converted, with all its game data files in place: nothing to restore. */
    fun inPlace(context: Context): Boolean {
        // Saves change as the game is played, so an iPhone game can always go back to its backup.
        if (isIphone) return false
        val installed = runCatching {
            context.packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0)).longVersionCode
        }.getOrNull()
        return installed == versionCode && Games.isConverted(context, packageName) &&
            dataFiles.all { (name, size) -> File(Games.obbFolder(packageName), name).length() == size }
    }
}

/**
 * Backups in a folder picked with Android's folder picker, so it works with any storage app that
 * offers folders there (DAVx5's WebDAV mounts, Google Drive, a USB drive). Inside it:
 *
 * - "Game Bridge/Games/" is a copy of the phone's Game Bridge folder (every game file: Android,
 *   iPhone, Originals), kept exactly the same: a file gone from the phone goes from the copy too.
 *   "Game Bridge/Games.txt" lists each file in it with its size and fingerprint (SHA-256). An SD
 *   card's or USB drive's Game Bridge folder goes to "Games on <drive id>" the same way.
 * - "Game Bridge/<package>/" holds an installed converted game's APKs (apk/), its game data files
 *   (obb/), its icon and info.txt, which is written last and lists every file with its size and
 *   fingerprint: a game whose files don't all match it is an unfinished backup.
 * - "Game Bridge/ios/<name>/" holds an iPhone game's saves (save/, its home folder without caches),
 *   its icon and info.txt the same way.
 *
 * A file is copied only when its fingerprint differs from the one the backup has; a file's
 * fingerprint is worked out again only when its size or modified time changed (saves: every time).
 * Backups from before the Game Bridge folder was copied also kept original files with each game
 * (original/, app/); those go once the folder's copy holds every file in them.
 *
 * Process-wide like Conversions, so a backup keeps going when the screen is rebuilt.
 */
object Backups {
    /** What is happening now ("Backing up Pirates · 45%"), or null when nothing runs. */
    var status by mutableStateOf<String?>(null)
        private set

    /** While a backup runs: what is being copied, how far the whole backup is, and where it is ("Game 2 of 9"). */
    class Progress(val label: String, val percent: Int, val detail: String)
    var progress by mutableStateOf<Progress?>(null)
        private set

    /** The last finished run's problem, or null when it went well. */
    var problem by mutableStateOf<String?>(null)
        private set

    /** The last run was stopped with Cancel: what it copied stays, and the next backup goes on from there. */
    var stopped by mutableStateOf(false)
        private set

    /** Cancel was pressed: the backup ends after the piece it is copying. */
    var stopping by mutableStateOf(false)
        private set

    @Volatile
    private var cancelled = false

    private class Stopped : Exception("stopped")

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private const val PREFERENCES = "backups"
    private const val FOLDER = "folder"
    private const val FOLDER_NAME = "folder_name"
    private const val LAST = "last"
    private const val LAST_GAMES = "last_games"
    private const val LAST_SIZE = "last_size"
    private const val TOP = "Game Bridge"
    private const val INFO = "info.txt"
    private const val COPY = "Games"

    private fun preferences(context: Context) = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    /** The picked folder (a document tree), if any and still allowed. */
    fun folder(context: Context): Uri? {
        val uri = preferences(context).getString(FOLDER, null)?.let(Uri::parse) ?: return null
        return uri.takeIf { context.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isWritePermission } }
    }

    /** Keeps the folder the picker returned, with lasting access to it. */
    fun setFolder(context: Context, tree: Uri) {
        context.contentResolver.takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        preferences(context).edit().putString(FOLDER, tree.toString()).remove(FOLDER_NAME).apply()
    }

    /** The folder's name from the last [folderName] call, read without asking the folder's app. */
    fun savedFolderName(context: Context): String? = preferences(context).getString(FOLDER_NAME, null)

    /**
     * The folder's app and name, like "RSAF › backups", and saves it for [savedFolderName].
     * Asks the folder's app, which can take many seconds for a cloud folder, so never call it on the main thread.
     */
    fun folderName(context: Context, tree: Uri): String {
        val app = context.packageManager.resolveContentProvider(tree.authority ?: "", PackageManager.ComponentInfoFlags.of(0))
            ?.applicationInfo?.loadLabel(context.packageManager)?.toString()
        val name = runCatching { displayName(context, DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))) }.getOrNull()
        val full = listOfNotNull(app, name).joinToString(" › ").ifEmpty { "the picked folder" }
        // Only a name the folder's app gave is kept, so a cloud folder that is offline doesn't replace a good one.
        if (name != null) preferences(context).edit().putString(FOLDER_NAME, full).apply()
        return full
    }

    /** When the last backup finished (milliseconds), or 0. */
    fun lastBackup(context: Context) = preferences(context).getLong(LAST, 0)

    /** How many games the last finished backup held, and its size in bytes (0 before the first). */
    fun lastGames(context: Context) = preferences(context).getInt(LAST_GAMES, 0)
    fun lastSize(context: Context) = preferences(context).getLong(LAST_SIZE, 0)

    /** Installed games Game Bridge converted, each with the original file it came from when it is on the phone. */
    fun convertedGames(context: Context, games: List<Game>): List<Pair<String, File?>> =
        context.packageManager.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
            .map { it.packageName }
            .filter { it != context.packageName && Games.isConverted(context, it) }
            .map { packageName -> packageName to games.firstOrNull { it.packageName == packageName && it.file != null }?.file }
            .sortedBy { it.first }

    /** An unpacked iPhone game: its home folder name (files/ios/<name>, where its saves are), and its game folder when found. */
    class IphoneSave(val name: String, val label: String, val bundleId: String, val app: File?, val icon: Bitmap?)

    /** iPhone games with saves on this phone, or found among `games` (each once, by home folder name). */
    fun iphoneGames(context: Context, games: List<Game>): List<IphoneSave> {
        val found = games.filter { it.action == Action.PlayIphone && it.file != null }.associateBy { it.file!!.nameWithoutExtension }
        val saves = iosHomes(context).listFiles().orEmpty().filter { it.isDirectory }.map { it.name }
        return (saves + found.keys).distinct().sorted().map { name ->
            val game = found[name]
            IphoneSave(name, game?.label ?: name, game?.packageName ?: name, game?.file, game?.icon)
        }
    }

    /** Where iPhone games keep their home folders (IosGameActivity gives each files/ios/<name>). */
    private fun iosHomes(context: Context) = File(context.filesDir, "ios")

    /** A home folder's files worth keeping: Documents and Library without Library/Caches, as iOS backs up (tmp and our run log stay out). */
    private fun saveFiles(home: File): List<File> =
        home.walkTopDown().onFail { _, failure -> throw failure }.filter { it.isFile }.filter { file ->
            val path = file.relativeTo(home).invariantSeparatorsPath
            (path.startsWith("Documents/") || path.startsWith("Library/")) && !path.startsWith("Library/Caches/")
        }.toList()

    /** A drive's Game Bridge folder, and the name of its copy in the backup ("Games", "Games on 1A2B-3C4D"); its list of files is "<copy>.txt". */
    private class Drive(val folder: File, val copy: String) {
        val index get() = "$copy.txt"
    }

    /** The phone's own storage, then each SD card or USB drive that is in (one taken out keeps its copy as it was). */
    private fun drives(context: Context): List<Drive> {
        val phone = Environment.getExternalStorageDirectory()
        val others = context.getSystemService(StorageManager::class.java).storageVolumes.mapNotNull { volume ->
            val root = volume.directory?.takeIf { it != phone } ?: return@mapNotNull null
            volume.uuid?.let { Drive(File(root, Games.FOLDER), "$COPY on $it") }
        }
        return listOf(Drive(File(phone, Games.FOLDER), COPY)) + others
    }

    /** Ends a running backup after the piece it is copying. */
    fun cancel() {
        if (status == null) return
        cancelled = true
        stopping = true
    }

    /** Backs up the Game Bridge folder, every converted game and every iPhone game's saves. Runs in the background; `status` follows it. */
    fun backUp(context: Context, games: List<Pair<String, File?>>, iphone: List<IphoneSave>, onDone: () -> Unit = {}) {
        if (status != null || Conversions.moving) return
        val appContext = context.applicationContext
        status = "Getting ready…"
        problem = null
        stopped = false
        cancelled = false
        scope.launch {
            val result = withContext(Dispatchers.IO) { runCatching { backUpAll(appContext, games, iphone) } }
            val failure = result.exceptionOrNull()
            stopped = failure is Stopped
            problem = failure?.takeIf { it !is Stopped }?.let { "The backup stopped: ${it.message ?: it.javaClass.simpleName}" }
            result.getOrNull()?.let { (count, size) ->
                preferences(appContext).edit().putLong(LAST, System.currentTimeMillis()).putInt(LAST_GAMES, count).putLong(LAST_SIZE, size).apply()
            }
            progress = null
            stopping = false
            // Reading for a restore stops on it too.
            cancelled = false
            status = null
            onDone()
        }
    }

    /** One file to save: its name in the game's folder and the file. `fresh`: its fingerprint is always worked out again (saves can change keeping their size and time). */
    private class Item(val path: String, val file: File, val fresh: Boolean = false) {
        val size = file.length()
    }

    /** One game's backup: its folder below "Game Bridge", the lines that start its info.txt, its files, and where its game folder or original file is. */
    private class Plan(val folder: String, val label: String, val header: List<String>, val items: List<Item>, val place: File?, val icon: () -> Bitmap?)

    /** A game backed up in this run: its folder, header, and the files its info.txt lists (size and fingerprint; old copies have no fingerprint). */
    private class Saved(val folder: Uri, val plan: Plan, val files: Map<String, Long>, val hashes: Map<String, String>)

    /** Files of backups from before the Game Bridge folder was copied: Android games' original files and iPhone game folders. */
    private fun isOldCopy(path: String) = path.startsWith("original/") || path.startsWith("app/")

    private fun hex(digest: MessageDigest) = digest.digest().joinToString("") { "%02x".format(it) }

    /** Copies `input` to `output` (or only reads it, for null), feeding `digest`; stops between pieces when Cancel was pressed. Returns the bytes read. */
    private fun pump(input: InputStream, output: OutputStream?, digest: MessageDigest, counted: (Long) -> Unit): Long {
        val buffer = ByteArray(1 shl 20)
        var total = 0L
        while (true) {
            if (cancelled) throw Stopped()
            val count = input.read(buffer)
            if (count < 0) break
            output?.write(buffer, 0, count)
            digest.update(buffer, 0, count)
            total += count
            counted(count.toLong())
        }
        return total
    }

    /**
     * Each file's fingerprint from the last time it was read, with its size and modified time then:
     * while both stay the same, the file isn't read again. Kept in the app's own files.
     */
    private class Fingerprints(private val file: File) {
        private class Known(val size: Long, val modified: Long, val hash: String)

        private val known = HashMap<String, Known>()

        init {
            runCatching {
                file.readLines().forEach { line ->
                    val parts = line.split(' ', limit = 4)
                    val size = parts.getOrNull(0)?.toLongOrNull()
                    val modified = parts.getOrNull(1)?.toLongOrNull()
                    if (parts.size == 4 && size != null && modified != null) known[parts[3]] = Known(size, modified, parts[2])
                }
            }
        }

        fun cached(source: File): String? = known[source.path]?.takeIf { it.size == source.length() && it.modified == source.lastModified() }?.hash

        /** Remembers what was read from `source`, when it didn't change while being read. */
        fun learn(source: File, size: Long, modified: Long, read: Long, hash: String) {
            if (read == size && source.length() == size && source.lastModified() == modified) known[source.path] = Known(size, modified, hash)
        }

        /** Saved for the next backup, without files no longer there. */
        fun save() {
            val text = buildString {
                known.filterKeys { File(it).isFile }.forEach { (path, entry) -> appendLine("${entry.size} ${entry.modified} ${entry.hash} $path") }
            }
            val partial = File(file.parentFile, "${file.name}.part")
            partial.writeText(text)
            partial.renameTo(file)
        }
    }

    /** `source`'s fingerprint: the remembered one while its size and time are the same (not for `fresh` files), else read now. */
    private fun fingerprint(source: File, fresh: Boolean, fingerprints: Fingerprints): String {
        if (!fresh) fingerprints.cached(source)?.let { return it }
        val size = source.length()
        val modified = source.lastModified()
        val digest = MessageDigest.getInstance("SHA-256")
        val read = source.inputStream().use { pump(it, null, digest) {} }
        return hex(digest).also { if (!fresh) fingerprints.learn(source, size, modified, read, it) }
    }

    /** Every file in `folder`, by its path below it. A folder that can't be read stops the backup: the copy must never take it for an empty one. */
    private fun walk(folder: File, prefix: String, into: MutableMap<String, File>) {
        val entries = folder.listFiles() ?: throw IllegalStateException("couldn't read ${folder.path}")
        for (entry in entries) {
            if (entry.isDirectory) walk(entry, "$prefix${entry.name}/", into) else into["$prefix${entry.name}"] = entry
        }
    }

    /** The folders `path` ("iPhone/Game.app/a.png") is in, top first ("iPhone", "iPhone/Game.app"). */
    private fun parents(path: String): List<String> =
        path.split('/').dropLast(1).runningReduce { above, name -> "$above/$name" }

    /** What the progress shows for a file in the Game Bridge folder: its game ("SwordGame" for iPhone/2/SwordGame.app/…) or file name. */
    private fun entryName(path: String): String {
        val names = path.split('/')
        return names.drop(1).dropLast(1).firstOrNull { name -> name.any { !it.isDigit() } }?.removeSuffix(".app") ?: names.last()
    }

    /** Returns how many games it backed up and the size of everything backed up, in bytes. */
    private fun backUpAll(context: Context, games: List<Pair<String, File?>>, iphone: List<IphoneSave>): Pair<Int, Long> {
        val tree = folder(context) ?: throw IllegalStateException("pick a backup folder first")
        val top = child(context, tree, root(tree), TOP, directory = true)
        val packages = context.packageManager
        val plans = games.map { (packageName, original) ->
            val application = packages.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
            val info = packages.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
            val label = application.loadLabel(packages).toString()
            val items = mutableListOf<Item>()
            (listOf(File(application.sourceDir)) + application.splitSourceDirs.orEmpty().map(::File)).forEach { apk ->
                items += Item("apk/${apk.name}", apk)
            }
            // A data folder that can't be read stops the backup (its earlier backup's data files would go).
            val obb = Games.obbFolder(packageName)
            (if (obb.exists()) obb.listFiles() ?: throw IllegalStateException("couldn't read ${obb.path}") else emptyArray()).filter { it.isFile }.forEach { data ->
                items += Item("obb/${data.name}", data)
            }
            val header = listOfNotNull("package=$packageName", "label=$label", "versionCode=${info.longVersionCode}",
                "versionName=${info.versionName ?: ""}", "installed=${info.lastUpdateTime}", original?.let { "original=${it.path}" })
            Plan(packageName, label, header, items, original) { runCatching { Games.iconBitmap(application.loadIcon(packages)) }.getOrNull() }
        } + iphone.mapNotNull { game ->
            val home = File(iosHomes(context), game.name)
            val items = saveFiles(home).map { file -> Item("save/${file.relativeTo(home).invariantSeparatorsPath}", file, fresh = true) }
            // A game never played has no saves; its game folder is in the Game Bridge folder's copy.
            if (items.isEmpty()) return@mapNotNull null
            val header = listOfNotNull("kind=iphone", "package=${game.bundleId}", "label=${game.label}", "name=${game.name}",
                game.app?.let { "place=${it.path}" }, "saved=${System.currentTimeMillis()}")
            Plan("ios/${game.name}", game.label, header, items, game.app) { game.icon }
        }
        // Every drive's Game Bridge folder, read whole before anything in its copy changes.
        val folders = drives(context).filter { it.folder.isDirectory }.associateWith { drive -> HashMap<String, File>().also { walk(drive.folder, "", it) } }
        val folderBytes = folders.values.sumOf { files -> files.values.sumOf { it.length() } }
        val total = (plans.sumOf { plan -> plan.items.sumOf { it.size } } + folderBytes).coerceAtLeast(1)
        val fingerprints = Fingerprints(File(context.filesDir, "backup-fingerprints.txt"))

        var label = ""
        var detail = ""
        var done = 0L
        var shown = -1
        fun show(force: Boolean = false) {
            val percent = (done * 100 / total).toInt().coerceIn(0, 100)
            if (percent == shown && !force) return
            shown = percent
            val (text, where) = label to detail
            scope.launch {
                status = "Backing up $text · $percent%"
                progress = Progress(text, percent, where)
            }
        }
        fun add(bytes: Long) {
            done += bytes
            show()
        }
        /** Copies `source` into the document `target`; the size and fingerprint of what it wrote. */
        fun copy(source: File, target: Uri, fresh: Boolean): Pair<Long, String> {
            val size = source.length()
            val modified = source.lastModified()
            val digest = MessageDigest.getInstance("SHA-256")
            val written = context.contentResolver.openOutputStream(target, "wt")!!.use { output ->
                source.inputStream().use { input -> pump(input, output, digest, ::add) }
            }
            // The fingerprint of the bytes written, so the backup's list always tells what the backup holds.
            return (written to hex(digest)).also { (_, hash) -> if (!fresh) fingerprints.learn(source, size, modified, written, hash) }
        }

        try {
            val saved = mutableListOf<Saved>()
            for ((index, plan) in plans.withIndex()) {
                if (cancelled) throw Stopped()
                label = plan.label
                detail = "Game ${index + 1} of ${plans.size}"
                show(force = true)
                val items = plan.items
                val gameFolder = child(context, tree, top, plan.folder, directory = true)
                val savedInfo = readInfo(context, tree, gameFolder)
                val before = savedInfo?.files.orEmpty()
                val hashes = savedInfo?.hashes.orEmpty()
                val present = listing(context, tree, gameFolder, "")
                // Already in the earlier, finished backup (info.txt exists only for finished ones): the same size there
                // (or a size the storage app doesn't tell) and the same fingerprint. A backup from before fingerprints is copied again.
                val same = items.filter { item ->
                    val hash = hashes[item.path]
                    hash != null && before[item.path] == item.size && present[item.path].let { it == item.size || it == -1L } &&
                        fingerprint(item.file, item.fresh, fingerprints) == hash
                }.mapTo(HashSet()) { it.path }
                // Changing files would leave the old info.txt calling a half-changed backup finished.
                if (items.any { it.path !in same }) find(context, tree, gameFolder, INFO)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
                // Folders already found or made, so a game folder with hundreds of files isn't listed again for each.
                val made = HashMap<String, Uri>()
                val files = LinkedHashMap<String, Long>()
                val written = HashMap<String, String>()
                for (item in items) {
                    if (cancelled) throw Stopped()
                    if (item.path in same) {
                        files[item.path] = item.size
                        written[item.path] = hashes.getValue(item.path)
                        add(item.size)
                        continue
                    }
                    val folderPath = item.path.substringBeforeLast('/')
                    val parent = made.getOrPut(folderPath) { child(context, tree, gameFolder, folderPath, directory = true) }
                    val name = item.path.substringAfterLast('/')
                    // A file the backup doesn't have yet is made without listing its folder first.
                    val target = if (item.path in present) child(context, tree, parent, name, directory = false) else make(context, parent, name, directory = false)
                    val (size, hash) = copy(item.file, target, item.fresh)
                    files[item.path] = size
                    written[item.path] = hash
                }
                // Files of an earlier backup this one doesn't have (an old version's data file, a deleted save) go.
                // Old copies of original files stay until the Game Bridge folder's copy holds them.
                present.keys.filter { it !in files && it != INFO && it != "icon.png" && !isOldCopy(it) }.forEach { path ->
                    find(context, tree, gameFolder, path)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
                }
                plan.icon()?.let { picture ->
                    val target = child(context, tree, gameFolder, "icon.png", directory = false)
                    context.contentResolver.openOutputStream(target, "wt")!!.use { picture.compress(Bitmap.CompressFormat.PNG, 100, it) }
                }
                // Old copies stay listed (at the size the storage app tells, else the one the earlier info.txt had), so they can still be restored.
                present.filterKeys { isOldCopy(it) }.forEach { (path, size) -> (size.takeIf { it >= 0 } ?: before[path])?.let { files[path] = it } }
                writeInfo(context, tree, gameFolder, plan.header, files, written)
                saved += Saved(gameFolder, plan, files, written)
            }

            // The Game Bridge folder of each drive.
            val copied = HashMap<Drive, Map<String, Pair<Long, String>>>()
            for ((drive, local) in folders) {
                label = "game files"
                detail = "Game Bridge folder"
                show(force = true)
                copied[drive] = mirror(context, tree, top, drive, local, fingerprints, ::copy, ::add) { name ->
                    if (name != label) {
                        label = name
                        show(force = true)
                    }
                }
            }

            // Old copies of original files go once the folder's copy holds every file in them.
            label = "old copies"
            detail = "Checking them against the folder's copy"
            show(force = true)
            for (game in saved) removeOldCopies(context, tree, game, copied)

            return plans.size to plans.sumOf { plan -> plan.items.sumOf { it.size } } + folderBytes
        } finally {
            runCatching { fingerprints.save() }
        }
    }

    /**
     * Makes the backup's copy of a drive's Game Bridge folder the same as `local` (its files by path):
     * copies the files that differ, removes what the phone no longer has. Returns the copy's list.
     */
    private fun mirror(
        context: Context, tree: Uri, top: Uri, drive: Drive, local: Map<String, File>, fingerprints: Fingerprints,
        copy: (File, Uri, Boolean) -> Pair<Long, String>, add: (Long) -> Unit, showing: (String) -> Unit,
    ): Map<String, Pair<Long, String>> {
        val copyFolder = child(context, tree, top, drive.copy, directory = true)
        val files = HashMap<String, Entry>()
        val folders = hashMapOf("" to copyFolder)
        fun list(parent: Uri, prefix: String) {
            for (entry in children(context, tree, parent)) {
                val path = "$prefix${entry.name}"
                if (entry.directory) {
                    folders[path] = entry.uri
                    list(entry.uri, "$path/")
                } else {
                    files[path] = entry
                }
            }
        }
        list(copyFolder, "")
        val indexDocument = find(context, tree, top, drive.index)
        val index = indexDocument?.let { readIndex(context, it).files }.orEmpty()

        // Unchanged: listed with the same size and fingerprint, and there at that size (or a size the storage app doesn't tell).
        val changed = local.filter { (path, file) ->
            if (cancelled) throw Stopped()
            val size = file.length()
            val listed = index[path]
            val there = files[path]
            val same = listed != null && there != null && listed.first == size && (there.size == size || there.size == -1L) &&
                fingerprint(file, false, fingerprints) == listed.second
            if (same) add(size)
            !same
        }.toSortedMap()
        // The list keeps only files that stay as they are, saved before anything changes: a backup cut off
        // partway never vouches for a file it was rewriting.
        val kept = HashMap(index.filterKeys { it in local && it !in changed && it in files })
        var indexTarget = indexDocument
        // `finished`: the list holds every file of the folder (a last line "finished"); a game folder is restored from the copy only then.
        fun save(finished: Boolean) {
            val text = buildString {
                kept.toSortedMap().forEach { (path, entry) -> appendLine("${entry.first} ${entry.second} $path") }
                if (finished) appendLine(FINISHED)
            }
            val target = indexTarget ?: child(context, tree, top, drive.index, directory = false).also { indexTarget = it }
            context.contentResolver.openOutputStream(target, "wt")!!.use { it.write(text.toByteArray()) }
        }
        if (changed.isNotEmpty() || kept.size != index.size || indexDocument == null) save(finished = false)

        // What the phone no longer has: whole folders in one step each (the topmost one), then single files.
        val localFolders = local.keys.flatMapTo(HashSet()) { parents(it) }
        val goneFolders = folders.keys.filter { it != "" && it !in localFolders }.toSet()
        for (path in goneFolders.filter { folder -> parents("$folder/").dropLast(1).none { it in goneFolders } }) {
            if (cancelled) throw Stopped()
            DocumentsContract.deleteDocument(context.contentResolver, folders.getValue(path))
        }
        for (path in files.keys.filter { it !in local && parents(it).none { folder -> folder in goneFolders } }) {
            if (cancelled) throw Stopped()
            DocumentsContract.deleteDocument(context.contentResolver, files.getValue(path).uri)
        }
        folders.keys.removeAll(goneFolders)
        files.keys.retainAll(local.keys)

        fun folder(path: String): Uri = folders[path] ?: make(context, folder(path.substringBeforeLast('/', "")), path.substringAfterLast('/'), directory = true)
            .also { folders[path] = it }
        var lastSaved = System.currentTimeMillis()
        try {
            for ((path, file) in changed) {
                if (cancelled) throw Stopped()
                showing(entryName(path))
                val target = files[path]?.uri ?: make(context, folder(path.substringBeforeLast('/', "")), path.substringAfterLast('/'), directory = false)
                kept[path] = copy(file, target, false)
                // Saved now and then, so a long copy cut off partway doesn't start over.
                if (System.currentTimeMillis() - lastSaved > 30_000) {
                    save(finished = false)
                    lastSaved = System.currentTimeMillis()
                }
            }
        } catch (failure: Throwable) {
            runCatching { save(finished = false) }
            throw failure
        }
        // Not saved is a failed backup: old copies of original files stay until the list is safely written.
        save(finished = true)
        return kept
    }

    /**
     * Removes a game's old copies of its original files (original/, app/) once the Game Bridge
     * folder's copy holds every file in them: info.txt is written without them first, so a backup
     * cut off between the two never lists a file that is gone.
     */
    private fun removeOldCopies(context: Context, tree: Uri, game: Saved, copied: Map<Drive, Map<String, Pair<Long, String>>>) {
        val old = game.files.filterKeys { isOldCopy(it) }
        if (old.isEmpty()) return
        val place = game.plan.place
        // The game folder's path in its drive's copy, wherever Game Bridge moved it since.
        val appPath = place?.let { Games.current(context, it) }?.let { folder ->
            copied.entries.firstOrNull { folder.path.startsWith(it.key.folder.path + "/") }?.let { (drive, list) -> list to folder.relativeTo(drive.folder).invariantSeparatorsPath }
        }
        val originals = copied.values.flatMap { list -> list.map { (path, entry) -> (path.substringAfterLast('/') to entry.first) to entry.second } }
            .groupBy({ it.first }, { it.second })
        // The fingerprints the copy has for the same file (the same name and size for an original file).
        fun matches(path: String, size: Long): List<String> = when {
            path.startsWith("original/") -> originals[path.substringAfterLast('/') to size].orEmpty()
            else -> listOfNotNull(appPath?.let { (list, folder) -> list["$folder/${path.removePrefix("app/")}"]?.takeIf { it.first == size }?.second })
        }
        // Old copies have no fingerprints, so each file is read once from the backup to compare.
        val documents by lazy { documents(context, tree, game.folder, "") }
        fun inCopy(path: String, size: Long): Boolean {
            val wanted = matches(path, size).ifEmpty { return false }
            val document = documents[path] ?: return false
            val digest = MessageDigest.getInstance("SHA-256")
            context.contentResolver.openInputStream(document)!!.use { pump(it, null, digest) {} }
            return hex(digest) in wanted
        }
        val gone = old.entries.groupBy { it.key.substringBefore('/') }.filterValues { files -> files.all { (path, size) -> inCopy(path, size) } }.keys
        if (gone.isEmpty()) return
        writeInfo(context, tree, game.folder, game.plan.header, game.files.filterKeys { it.substringBefore('/') !in gone }, game.hashes)
        gone.forEach { name -> find(context, tree, game.folder, name)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) } }
    }

    /** Writes a game's info.txt: its header, then each file's size and fingerprint. */
    private fun writeInfo(context: Context, tree: Uri, gameFolder: Uri, header: List<String>, files: Map<String, Long>, hashes: Map<String, String>) {
        val text = buildString {
            header.forEach(::appendLine)
            files.forEach { (path, size) -> appendLine("file=$size $path") }
            files.keys.forEach { path -> hashes[path]?.let { appendLine("hash=$it $path") } }
        }
        val target = child(context, tree, gameFolder, INFO, directory = false)
        context.contentResolver.openOutputStream(target, "wt")!!.use { it.write(text.toByteArray()) }
    }

    /** A copy's list: each file's path to its size and fingerprint, and whether it holds the whole folder. */
    private class Index(val files: Map<String, Pair<Long, String>>, val finished: Boolean)

    private const val FINISHED = "finished"

    /** Lines a cut-off write left broken are left out. */
    private fun readIndex(context: Context, document: Uri): Index {
        val lines = context.contentResolver.openInputStream(document)!!.use { it.readBytes().decodeToString() }.lines()
        val files = lines.mapNotNull { line ->
            val parts = line.split(' ', limit = 3)
            val size = parts.getOrNull(0)?.toLongOrNull()
            if (parts.size == 3 && size != null && parts[1].length == 64 && parts[2].isNotEmpty()) parts[2] to (size to parts[1]) else null
        }.toMap()
        return Index(files, lines.any { it == FINISHED })
    }

    /** The games in the backup folder whose backups are complete. */
    fun list(context: Context): List<SavedGame> {
        val tree = folder(context) ?: return emptyList()
        val top = find(context, tree, root(tree), TOP) ?: return emptyList()
        // The Game Bridge folder's copies hold game files, not games' backups.
        val folders = children(context, tree, top).filter { it.directory && it.name != COPY && !it.name.startsWith("$COPY on ") }
        val iphone = folders.firstOrNull { it.name == "ios" }?.let { children(context, tree, it.uri).filter { entry -> entry.directory } }.orEmpty()
        return (folders + iphone).mapNotNull { folder ->
            val info = readInfo(context, tree, folder.uri) ?: return@mapNotNull null
            val present = listing(context, tree, folder.uri, "")
            // Some storage apps don't tell sizes (-1): then the file being there has to do.
            if (info.files.isEmpty() || info.files.any { (path, size) -> present[path].let { it == null || (it >= 0 && it != size) } }) return@mapNotNull null
            val icon = find(context, tree, folder.uri, "icon.png")?.let { uri ->
                runCatching { context.contentResolver.openInputStream(uri)!!.use(BitmapFactory::decodeStream) }.getOrNull()
            }
            val data = info.files.filterKeys { it.startsWith("obb/") }.mapKeys { it.key.removePrefix("obb/") }
            val saves = if (info.kind == "iphone") info.saved ?: 0 else null
            SavedGame(info.packageName, info.label, info.versionCode, info.versionName, icon, info.files.values.sum(), folder.uri, data,
                saves, info.name, info.place)
        }.sortedBy { it.label.lowercase() }
    }

    /** A path from a backup's list that stays inside the folder it is copied into. */
    private fun safe(path: String) = !path.startsWith("/") && path.split('/').none { it == ".." }

    /** Copies backup documents to files on the phone, checking each one's size and fingerprint (when the backup has one) and showing how far it is. */
    private class Fetch(val context: Context, total: Long, val onProgress: (String) -> Unit) {
        private val total = total.coerceAtLeast(1)
        private var done = 0L
        private var shown = -1

        fun add(bytes: Long) {
            done += bytes
            val percent = (done * 100 / total).toInt()
            if (percent != shown) {
                shown = percent
                onProgress("Copying back · $percent%")
            }
        }

        fun copy(source: Uri, target: File, size: Long, hash: String?) {
            target.parentFile!!.mkdirs()
            val digest = MessageDigest.getInstance("SHA-256")
            val buffer = ByteArray(1 shl 20)
            context.contentResolver.openInputStream(source)!!.use { input ->
                target.outputStream().use { output ->
                    while (true) {
                        val count = input.read(buffer)
                        if (count < 0) break
                        output.write(buffer, 0, count)
                        digest.update(buffer, 0, count)
                        add(count.toLong())
                    }
                }
            }
            if (target.length() != size) throw IllegalStateException("${target.name} didn't copy completely")
            if (hash != null && hex(digest) != hash) throw IllegalStateException("${target.name} in the backup is damaged")
        }

        /** Copied under a .part name first, so a stopped copy never looks like the finished file or folder. */
        fun place(target: File, fill: (File) -> Unit) {
            val partial = File(target.parentFile, "${target.name}.part").apply { deleteRecursively() }
            try {
                fill(partial)
                if (!partial.renameTo(target)) throw IllegalStateException("${target.name} couldn't be put in place")
            } finally {
                partial.deleteRecursively()
            }
        }
    }

    /**
     * A file or folder's copy in the backup's copy of its drive's Game Bridge folder: where it goes
     * on the phone, each file's path below it ("" for a file) with its size and fingerprint, and its document.
     */
    private class InCopy(val target: File, val files: Map<String, Pair<Long, String>>, val document: Uri)

    /**
     * Where `target` (a file or folder in a Game Bridge folder) is in the backup's copy, when the copy
     * has it whole: only a copy whose last backup finished holds every file of a game folder.
     */
    private fun inCopy(context: Context, tree: Uri, top: Uri, target: File): InCopy? {
        val real = target.canonicalFile
        val drive = drives(context).firstOrNull { real.path.startsWith(it.folder.canonicalPath + "/") } ?: return null
        val path = real.relativeTo(drive.folder.canonicalFile).invariantSeparatorsPath
        val index = find(context, tree, top, drive.index)?.let { readIndex(context, it) }?.takeIf { it.finished } ?: return null
        val files = index.files.filter { (listed, _) -> listed == path || listed.startsWith("$path/") }
            .mapKeys { (listed, _) -> listed.removePrefix(path).removePrefix("/") }
        if (files.isEmpty() || !files.keys.all(::safe)) return null
        val document = find(context, tree, top, "${drive.copy}/$path") ?: return null
        return InCopy(real, files, document)
    }

    /** Installs a saved game, puts its data files back and its original file in the Game Bridge folder (if not there). Null on success, else a plain reason. */
    fun restore(context: Context, game: SavedGame, onProgress: (String) -> Unit): String? {
        val tree = folder(context) ?: return "Pick the backup folder again."
        if (game.isIphone) return restoreIphone(context, tree, game, onProgress)
        val work = File(context.cacheDir, "restore").apply { deleteRecursively(); mkdirs() }
        try {
            val info = readInfo(context, tree, game.folder) ?: return "The backup of ${game.label} is incomplete."
            val files = info.files
            if (!files.keys.all(::safe)) return "The backup of ${game.label} is damaged."
            val top = find(context, tree, root(tree), TOP) ?: return "The backup of ${game.label} is incomplete."
            // Its original file from the Game Bridge folder's copy, when it is no longer on the phone.
            val original = info.original?.let(::File)?.takeIf { !it.isFile }?.let { file -> inCopy(context, tree, top, file)?.let { file to it } }
            val fetch = Fetch(context, files.values.sum() + (original?.second?.files?.values?.sumOf { it.first } ?: 0), onProgress)
            fun copy(path: String, target: File) {
                val source = find(context, tree, game.folder, path) ?: throw IllegalStateException("$path is missing from the backup")
                fetch.copy(source, target, files.getValue(path), info.hashes[path])
            }
            val installed = runCatching {
                context.packageManager.getPackageInfo(game.packageName, PackageManager.PackageInfoFlags.of(0)).longVersionCode
            }.getOrNull()
            // This version already installed (only its data was missing): no need to install it again.
            if (installed != game.versionCode || !Games.isConverted(context, game.packageName)) {
                val apks = files.keys.filter { it.startsWith("apk/") }.sortedBy { if (it == "apk/base.apk") "" else it }
                val pieces = apks.map { path -> File(work, path).also { copy(path, it) } }
                onProgress("Installing…")
                when (val result = Installer(context).install(pieces)) {
                    is InstallResult.Done -> {}
                    is InstallResult.OtherCopy, is InstallResult.Conflict ->
                        return "Another copy of ${game.label} is installed. Remove it first (press and hold its card), then restore."
                    is InstallResult.NeedsTarget -> return "Android refused this backup of ${game.label}: it is made for an older Android."
                    is InstallResult.Failed -> return result.reason
                }
                pieces.forEach(File::delete)
            } else {
                fetch.add(files.filterKeys { it.startsWith("apk/") }.values.sum())
            }
            for (path in files.keys.filter { it.startsWith("obb/") }) {
                val target = File(Games.obbFolder(game.packageName), path.removePrefix("obb/"))
                val size = files.getValue(path)
                // One there at the same size is checked by its fingerprint when the backup has one.
                val same = target.length() == size && info.hashes[path].let { hash ->
                    hash == null || hex(MessageDigest.getInstance("SHA-256").also { digest -> target.inputStream().use { pump(it, null, digest) {} } }) == hash
                }
                if (same) fetch.add(size) else fetch.place(target) { copy(path, it) }
            }
            original?.let { (_, copy) ->
                val target = copy.target
                val (size, hash) = copy.files.getValue("")
                target.parentFile!!.mkdirs()
                fetch.place(target) { fetch.copy(copy.document, it, size, hash) }
            }
            // A backup from before the Game Bridge folder was copied keeps the original file with the game.
            for (path in files.keys.filter { it.startsWith("original/") }) {
                val target = File(Games.androidFolder(), path.removePrefix("original/"))
                if (target.length() != files[path] && (original == null || original.first.name != target.name)) {
                    target.parentFile!!.mkdirs()
                    fetch.place(target) { copy(path, it) }
                }
            }
            return null
        } catch (failure: Exception) {
            return "Couldn't restore ${game.label}: ${failure.message}"
        } finally {
            work.deleteRecursively()
        }
    }

    /**
     * Puts an iPhone game's saves back as they were backed up (the game is closed first), and its
     * game folder in the Game Bridge folder when it's no longer on the phone. The saves it replaces
     * are kept in files/ios-replaced/<name> until the next restore of that game.
     */
    private fun restoreIphone(context: Context, tree: Uri, game: SavedGame, onProgress: (String) -> Unit): String? {
        val name = game.homeName ?: return "The backup of ${game.label} is incomplete."
        if (name.isEmpty() || '/' in name || name == "." || name == "..") return "The backup of ${game.label} is damaged."
        val homes = iosHomes(context)
        val staging = File(homes, "$name.restoring").apply { deleteRecursively() }
        try {
            val info = readInfo(context, tree, game.folder) ?: return "The backup of ${game.label} is incomplete."
            val files = info.files
            if (!files.keys.all(::safe)) return "The backup of ${game.label} is damaged."
            val top = find(context, tree, root(tree), TOP) ?: return "The backup of ${game.label} is incomplete."
            // Where Game Bridge moved it since, when it did.
            val placed = game.place?.let { Games.current(context, File(it)) }
            val download = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "ios")
            val folderName = placed?.name?.takeIf { it != "." && it != ".." } ?: "$name.app"
            val there = (placed != null && File(placed, "Info.plist").isFile) ||
                listOf(Games.iphoneFolder(), download).any { File(File(it, folderName), "Info.plist").isFile }
            // The game folder comes back only when it isn't on the phone any more: from the Game Bridge
            // folder's copy, or for a backup from before that, the copy kept with the game (app/).
            val fromCopy = if (there || placed == null) null else inCopy(context, tree, top, placed)
            val appTarget = when {
                there -> null
                fromCopy != null -> fromCopy.target
                files.keys.any { it.startsWith("app/") } -> File(Games.iphoneFolder(), folderName)
                else -> null
            }
            val wanted = files.filterKeys { it.startsWith("save/") || (appTarget != null && fromCopy == null && it.startsWith("app/")) }
            val fetch = Fetch(context, wanted.values.sum() + (fromCopy?.files?.values?.sumOf { it.first } ?: 0), onProgress)
            // Every file's document, found in one pass (a game folder has hundreds).
            val documents = documents(context, tree, game.folder, "")
            fun copy(path: String, target: File) {
                val source = documents[path] ?: throw IllegalStateException("$path is missing from the backup")
                fetch.copy(source, target, files.getValue(path), info.hashes[path])
            }
            // Copied beside the game's home folder first, so a stopped copy leaves the current saves alone.
            for (path in wanted.keys.filter { it.startsWith("save/") }) copy(path, File(staging, path.removePrefix("save/")))
            staging.mkdirs()
            if (appTarget != null) {
                appTarget.parentFile!!.mkdirs()
                fetch.place(appTarget) { partial ->
                    if (fromCopy != null) {
                        val copyDocuments = documents(context, tree, fromCopy.document, "")
                        for ((path, entry) in fromCopy.files) {
                            val source = copyDocuments[path] ?: throw IllegalStateException("$path is missing from the backup")
                            fetch.copy(source, File(partial, path), entry.first, entry.second)
                        }
                    } else {
                        for (path in wanted.keys.filter { it.startsWith("app/") }) copy(path, File(partial, path.removePrefix("app/")))
                    }
                }
            }
            // A backup made before the game was ever played has no saves: the current ones stay.
            if (wanted.keys.none { it.startsWith("save/") }) return null
            onProgress("Closing the game…")
            closeIphoneGame(context)
            val home = File(homes, name)
            val replaced = File(File(context.filesDir, "ios-replaced"), name).apply { deleteRecursively(); parentFile!!.mkdirs() }
            if (home.exists() && !home.renameTo(replaced)) return "Couldn't restore ${game.label}: its current saves couldn't be moved aside."
            if (!staging.renameTo(home)) {
                replaced.renameTo(home)
                return "Couldn't restore ${game.label}: its saves couldn't be put in place."
            }
            return null
        } catch (failure: Exception) {
            return "Couldn't restore ${game.label}: ${failure.message}"
        } finally {
            staging.deleteRecursively()
        }
    }

    /** Stops the iPhone game screen's process (it runs apart from Game Bridge), so no game writes its saves during a restore. */
    private fun closeIphoneGame(context: Context) {
        val processes = context.getSystemService(android.app.ActivityManager::class.java)?.runningAppProcesses.orEmpty()
        val game = processes.firstOrNull { it.processName == "${context.packageName}:ios" } ?: return
        android.os.Process.killProcess(game.pid)
        // Gone before its home folder is swapped.
        val until = System.currentTimeMillis() + 3000
        while (File("/proc/${game.pid}").exists() && System.currentTimeMillis() < until) Thread.sleep(50)
    }

    // Documents in the picked folder's tree.

    private class Entry(val uri: Uri, val name: String, val directory: Boolean, val size: Long)

    private class Info(val packageName: String, val label: String, val versionCode: Long, val versionName: String, val installed: Long?, val files: Map<String, Long>,
        val hashes: Map<String, String>, val kind: String?, val name: String?, val place: String?, val saved: Long?, val original: String?)

    private fun root(tree: Uri) = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))

    private fun displayName(context: Context, document: Uri): String? =
        context.contentResolver.query(document, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { if (it.moveToFirst()) it.getString(0) else null }

    /** What is in `parent`. A folder that can't be listed is a problem, never an empty folder (the backup would take its files for gone). */
    private fun children(context: Context, tree: Uri, parent: Uri): List<Entry> {
        val list = DocumentsContract.buildChildDocumentsUriUsingTree(tree, DocumentsContract.getDocumentId(parent))
        val columns = arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE)
        return context.contentResolver.query(list, columns, null, null, null)?.use { cursor ->
            buildList {
                while (cursor.moveToNext()) {
                    add(Entry(DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0)), cursor.getString(1),
                        cursor.getString(2) == Document.MIME_TYPE_DIR, if (cursor.isNull(3)) -1 else cursor.getLong(3)))
                }
            }
        } ?: throw IllegalStateException("the backup folder didn't list ${displayName(context, parent) ?: "a folder"}")
    }

    /** Every file under `parent`, by its path below it, with its size. */
    private fun listing(context: Context, tree: Uri, parent: Uri, prefix: String): Map<String, Long> =
        children(context, tree, parent).flatMap { entry ->
            if (entry.directory) listing(context, tree, entry.uri, "$prefix${entry.name}/").toList() else listOf("$prefix${entry.name}" to entry.size)
        }.toMap()

    /** Every file under `parent`, by its path below it, with its document. */
    private fun documents(context: Context, tree: Uri, parent: Uri, prefix: String): Map<String, Uri> =
        children(context, tree, parent).flatMap { entry ->
            if (entry.directory) documents(context, tree, entry.uri, "$prefix${entry.name}/").toList() else listOf("$prefix${entry.name}" to entry.uri)
        }.toMap()

    /** The document at `path` ("obb/main.1.x.obb") below `parent`, if there. */
    private fun find(context: Context, tree: Uri, parent: Uri, path: String): Uri? =
        path.split('/').fold(parent as Uri?) { folder, name -> folder?.let { children(context, tree, it).firstOrNull { entry -> entry.name == name }?.uri } }

    /** The folder or file at `path` below `parent`, made when missing. */
    private fun child(context: Context, tree: Uri, parent: Uri, path: String, directory: Boolean): Uri =
        path.split('/').let { names ->
            names.foldIndexed(parent) { index, folder, name ->
                val isDirectory = directory || index < names.size - 1
                children(context, tree, folder).firstOrNull { it.name == name }?.uri ?: make(context, folder, name, isDirectory)
            }
        }

    /** A new folder or file `name` in `parent`. */
    private fun make(context: Context, parent: Uri, name: String, directory: Boolean): Uri =
        (DocumentsContract.createDocument(context.contentResolver, parent,
            if (directory) Document.MIME_TYPE_DIR else "application/octet-stream", name)
            ?: throw IllegalStateException("the backup folder refused to make $name")).also { made ->
            // Some storage apps rename new files (adding an ending or " (1)"); the backup must find it by name later.
            if (displayName(context, made) != name) {
                runCatching { DocumentsContract.deleteDocument(context.contentResolver, made) }
                throw IllegalStateException("the backup folder renamed $name, so it can't be used for backups")
            }
        }

    private fun readInfo(context: Context, tree: Uri, gameFolder: Uri): Info? {
        val uri = find(context, tree, gameFolder, INFO) ?: return null
        val lines = runCatching { context.contentResolver.openInputStream(uri)!!.use { it.readBytes().decodeToString().lines() } }.getOrNull() ?: return null
        val values = lines.mapNotNull { line -> line.split('=', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } }
        fun value(key: String) = values.firstOrNull { it.first == key }?.second
        fun pairs(key: String) = values.filter { it.first == key }.mapNotNull { (_, text) -> text.split(' ', limit = 2).takeIf { it.size == 2 }?.let { it[1] to it[0] } }
        val files = pairs("file").mapNotNull { (path, size) -> size.toLongOrNull()?.let { path to it } }.toMap()
        val hashes = pairs("hash").filter { it.second.length == 64 }.toMap()
        return Info(value("package") ?: return null, value("label") ?: "", value("versionCode")?.toLongOrNull() ?: 0, value("versionName") ?: "",
            value("installed")?.toLongOrNull(), files, hashes, value("kind"), value("name"), value("place"), value("saved")?.toLongOrNull(), value("original"))
    }
}
