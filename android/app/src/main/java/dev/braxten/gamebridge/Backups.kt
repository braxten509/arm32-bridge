package dev.braxten.gamebridge

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Environment
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
 * Backups of converted games in a folder picked with Android's folder picker, so it works with
 * any storage app that offers folders there (DAVx5's WebDAV mounts, Google Drive, a USB drive).
 * Inside it, "Game Bridge/<package>/" holds a game's installed APKs (apk/), its game data files
 * (obb/), optionally the original file it was converted from (original/), its icon, and
 * info.txt, which is written last and lists every file with its size: a game whose files don't
 * all match it is an unfinished backup. "Game Bridge/ios/<name>/" holds an unpacked iPhone game's
 * saves (save/, its home folder without caches), optionally the game folder itself (app/), its
 * icon and info.txt the same way.
 *
 * Process-wide like Conversions, so a backup keeps going when the screen is rebuilt.
 */
object Backups {
    /** What is happening now ("Backing up Pirates · 45%"), or null when nothing runs. */
    var status by mutableStateOf<String?>(null)
        private set

    /** The last finished run's problem, or null when it went well. */
    var problem by mutableStateOf<String?>(null)
        private set

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private const val PREFERENCES = "backups"
    private const val FOLDER = "folder"
    private const val FOLDER_NAME = "folder_name"
    private const val ORIGINALS = "originals"
    private const val LAST = "last"
    private const val TOP = "Game Bridge"
    private const val INFO = "info.txt"

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

    /** "DAVx5 › Game backups": the app that keeps the folder, then the folder's name. */
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

    var keepOriginals: Boolean = false
        private set

    fun loadChoices(context: Context) {
        keepOriginals = preferences(context).getBoolean(ORIGINALS, false)
    }

    fun setKeepOriginals(context: Context, keep: Boolean) {
        keepOriginals = keep
        preferences(context).edit().putBoolean(ORIGINALS, keep).apply()
    }

    /** When the last backup finished (milliseconds), or 0. */
    fun lastBackup(context: Context) = preferences(context).getLong(LAST, 0)

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
        home.walkTopDown().filter { it.isFile }.filter { file ->
            val path = file.relativeTo(home).invariantSeparatorsPath
            (path.startsWith("Documents/") || path.startsWith("Library/")) && !path.startsWith("Library/Caches/")
        }.toList()

    /** Backs up every converted game (and its original file when `originals`) and every iPhone game's saves (and its game folder when `originals`). Runs in the background; `status` follows it. */
    fun backUp(context: Context, games: List<Pair<String, File?>>, iphone: List<IphoneSave>, originals: Boolean, onDone: () -> Unit = {}) {
        if (status != null || Conversions.moving) return
        val appContext = context.applicationContext
        status = "Getting ready…"
        problem = null
        scope.launch {
            val failure = withContext(Dispatchers.IO) {
                runCatching { backUpAll(appContext, games, iphone, originals) }.exceptionOrNull()
            }
            problem = failure?.let { "The backup stopped: ${it.message ?: it.javaClass.simpleName}" }
            if (failure == null) preferences(appContext).edit().putLong(LAST, System.currentTimeMillis()).apply()
            status = null
            onDone()
        }
    }

    /** One file to save: its name in the game's folder, its size, and how to read it. `always`: copied even when the backup has it at this size (saves change in place). */
    private class Item(val path: String, val size: Long, val always: Boolean = false, val open: () -> InputStream)

    /** One game's backup: its folder below "Game Bridge", the lines that start its info.txt, and its files. */
    private class Plan(val folder: String, val label: String, val installed: Long?, val header: List<String>, val items: List<Item>, val icon: () -> Bitmap?)

    /** Files a backup without originals leaves alone: Android games' original files and iPhone game folders. */
    private fun isOriginal(path: String) = path.startsWith("original/") || path.startsWith("app/")

    private fun backUpAll(context: Context, games: List<Pair<String, File?>>, iphone: List<IphoneSave>, originals: Boolean) {
        val tree = folder(context) ?: throw IllegalStateException("pick a backup folder first")
        val top = child(context, tree, root(tree), TOP, directory = true)
        val packages = context.packageManager
        val plans = games.map { (packageName, original) ->
            val application = packages.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
            val info = packages.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
            val label = application.loadLabel(packages).toString()
            val items = mutableListOf<Item>()
            (listOf(File(application.sourceDir)) + application.splitSourceDirs.orEmpty().map(::File)).forEach { apk ->
                items += Item("apk/${apk.name}", apk.length()) { apk.inputStream() }
            }
            Games.obbFolder(packageName).listFiles().orEmpty().filter { it.isFile }.forEach { data ->
                items += Item("obb/${data.name}", data.length()) { data.inputStream() }
            }
            if (originals && original != null && original.isFile) items += Item("original/${original.name}", original.length()) { original.inputStream() }
            val header = listOf("package=$packageName", "label=$label", "versionCode=${info.longVersionCode}",
                "versionName=${info.versionName ?: ""}", "installed=${info.lastUpdateTime}")
            Plan(packageName, label, info.lastUpdateTime, header, items) { runCatching { Games.iconBitmap(application.loadIcon(packages)) }.getOrNull() }
        } + iphone.mapNotNull { game ->
            val home = File(iosHomes(context), game.name)
            val items = saveFiles(home).map { file ->
                Item("save/${file.relativeTo(home).invariantSeparatorsPath}", file.length(), always = true) { file.inputStream() }
            }.toMutableList()
            val app = game.app
            if (originals && app != null && app.isDirectory) {
                app.walkTopDown().filter { it.isFile }.forEach { file ->
                    items += Item("app/${file.relativeTo(app).invariantSeparatorsPath}", file.length()) { file.inputStream() }
                }
            }
            // A game never played and left out as an original has nothing to back up.
            if (items.isEmpty()) return@mapNotNull null
            val header = listOfNotNull("kind=iphone", "package=${game.bundleId}", "label=${game.label}", "name=${game.name}",
                app?.let { "place=${it.path}" }, "saved=${System.currentTimeMillis()}")
            Plan("ios/${game.name}", game.label, null, header, items) { game.icon }
        }
        val total = plans.sumOf { plan -> plan.items.sumOf { it.size } }.coerceAtLeast(1)
        var done = 0L
        for (plan in plans) {
            val label = plan.label
            val items = plan.items
            val gameFolder = child(context, tree, top, plan.folder, directory = true)
            val savedInfo = readInfo(context, tree, gameFolder)
            val saved = savedInfo?.files.orEmpty()
            // A game installed again (converted again, updated) has new APKs even when their sizes match.
            val sameInstall = savedInfo?.installed == plan.installed
            val present = listing(context, tree, gameFolder, "")
            // Already saved by the earlier, finished backup (info.txt exists only for finished ones):
            // same name and size (or a size the storage app doesn't tell), and for the APKs the same install.
            fun unchanged(item: Item) = !item.always && saved[item.path] == item.size && present[item.path].let { it == item.size || it == -1L } &&
                (sameInstall || !item.path.startsWith("apk/"))
            // Changing files would leave the old info.txt calling a half-changed backup finished.
            if (items.any { !unchanged(it) }) find(context, tree, gameFolder, INFO)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
            // Folders already found or made, so a game folder with hundreds of files isn't listed again for each.
            val folders = HashMap<String, Uri>()
            for (item in items) {
                if (unchanged(item)) {
                    done += item.size
                    continue
                }
                val folderPath = item.path.substringBeforeLast('/')
                val parent = folders.getOrPut(folderPath) { child(context, tree, gameFolder, folderPath, directory = true) }
                val name = item.path.substringAfterLast('/')
                // A file the backup doesn't have yet is made without listing its folder first.
                val target = if (item.path in present) child(context, tree, parent, name, directory = false) else make(context, parent, name, directory = false)
                context.contentResolver.openOutputStream(target, "wt")!!.use { output ->
                    item.open().use { input ->
                        val buffer = ByteArray(1 shl 20)
                        var lastShown = -1
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            output.write(buffer, 0, count)
                            done += count
                            val percent = (done * 100 / total).toInt()
                            if (percent != lastShown) {
                                lastShown = percent
                                scope.launch { status = "Backing up $label · $percent%" }
                            }
                        }
                    }
                }
            }
            // Files of an earlier backup this one doesn't have (an old version's data file, a deleted save) go,
            // except original files when this backup leaves them out.
            val kept = items.map { it.path }.toSet()
            present.keys.filter { it !in kept && it != INFO && it != "icon.png" && (originals || !isOriginal(it)) }.forEach { path ->
                find(context, tree, gameFolder, path)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
            }
            plan.icon()?.let { picture ->
                val target = child(context, tree, gameFolder, "icon.png", directory = false)
                context.contentResolver.openOutputStream(target, "wt")!!.use { picture.compress(Bitmap.CompressFormat.PNG, 100, it) }
            }
            // The sizes this backup wrote, and earlier original files kept as they are listed.
            val listed = items.associate { it.path to it.size } +
                listing(context, tree, gameFolder, "").filterKeys { isOriginal(it) && it !in kept }
                    .mapValues { (path, size) -> if (size >= 0) size else saved[path] ?: size }.filterValues { it >= 0 }
            val text = buildString {
                plan.header.forEach(::appendLine)
                listed.forEach { (path, size) -> appendLine("file=$size $path") }
            }
            val target = child(context, tree, gameFolder, INFO, directory = false)
            context.contentResolver.openOutputStream(target, "wt")!!.use { it.write(text.toByteArray()) }
        }
    }

    /** The games in the backup folder whose backups are complete. */
    fun list(context: Context): List<SavedGame> {
        val tree = folder(context) ?: return emptyList()
        val top = find(context, tree, root(tree), TOP) ?: return emptyList()
        val folders = children(context, tree, top).filter { it.directory }
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

    /** Installs a saved game, puts its data files back and its original file in Download (if not there). Null on success, else a plain reason. */
    fun restore(context: Context, game: SavedGame, onProgress: (String) -> Unit): String? {
        val tree = folder(context) ?: return "Pick the backup folder again."
        if (game.isIphone) return restoreIphone(context, tree, game, onProgress)
        val work = File(context.cacheDir, "restore").apply { deleteRecursively(); mkdirs() }
        try {
            val files = readInfo(context, tree, game.folder)?.files ?: return "The backup of ${game.label} is incomplete."
            val total = files.values.sum().coerceAtLeast(1)
            var done = 0L
            fun copy(path: String, target: File) {
                val source = find(context, tree, game.folder, path) ?: throw IllegalStateException("$path is missing from the backup")
                target.parentFile!!.mkdirs()
                context.contentResolver.openInputStream(source)!!.use { input ->
                    target.outputStream().use { output ->
                        val buffer = ByteArray(1 shl 20)
                        var lastShown = -1
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            output.write(buffer, 0, count)
                            done += count
                            val percent = (done * 100 / total).toInt()
                            if (percent != lastShown) {
                                lastShown = percent
                                onProgress("Copying back · $percent%")
                            }
                        }
                    }
                }
                if (target.length() != files[path]) throw IllegalStateException("$path didn't copy completely")
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
                done += files.filterKeys { it.startsWith("apk/") }.values.sum()
            }
            // Copied under a .part name first, so a stopped copy never looks like the finished file.
            fun place(path: String, target: File) {
                val partial = File(target.parentFile, "${target.name}.part")
                try {
                    copy(path, partial)
                    if (!partial.renameTo(target)) throw IllegalStateException("${target.name} couldn't be put in place")
                } finally {
                    partial.delete()
                }
            }
            for (path in files.keys.filter { it.startsWith("obb/") }) {
                val target = File(Games.obbFolder(game.packageName), path.removePrefix("obb/"))
                if (target.length() != files[path]) place(path, target)
            }
            val download = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
            for (path in files.keys.filter { it.startsWith("original/") }) {
                val target = File(download, path.removePrefix("original/"))
                if (target.length() != files[path]) place(path, target)
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
     * game folder in Game Bridge/iPhone when it's no longer on the phone. The saves it replaces
     * are kept in files/ios-replaced/<name> until the next restore of that game.
     */
    private fun restoreIphone(context: Context, tree: Uri, game: SavedGame, onProgress: (String) -> Unit): String? {
        val name = game.homeName ?: return "The backup of ${game.label} is incomplete."
        val homes = iosHomes(context)
        val staging = File(homes, "$name.restoring").apply { deleteRecursively() }
        try {
            val files = readInfo(context, tree, game.folder)?.files ?: return "The backup of ${game.label} is incomplete."
            // Where Game Bridge moved it since, when it did.
            val placed = game.place?.let { Games.current(context, File(it)) }
            val download = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "ios")
            val folderName = placed?.name ?: "$name.app"
            // The game folder comes back only when it isn't on the phone any more.
            val appTarget = when {
                files.keys.none { it.startsWith("app/") } -> null
                placed != null && File(placed, "Info.plist").isFile -> null
                listOf(Games.iphoneFolder(), download).any { File(File(it, folderName), "Info.plist").isFile } -> null
                else -> File(Games.iphoneFolder(), folderName)
            }
            val wanted = files.filterKeys { it.startsWith("save/") || (appTarget != null && it.startsWith("app/")) }
            val total = wanted.values.sum().coerceAtLeast(1)
            var done = 0L
            // Every file's document, found in one pass (a game folder has hundreds).
            val documents = documents(context, tree, game.folder, "")
            fun copy(path: String, target: File) {
                val source = documents[path] ?: throw IllegalStateException("$path is missing from the backup")
                target.parentFile!!.mkdirs()
                context.contentResolver.openInputStream(source)!!.use { input ->
                    target.outputStream().use { output ->
                        val buffer = ByteArray(1 shl 20)
                        var lastShown = -1
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            output.write(buffer, 0, count)
                            done += count
                            val percent = (done * 100 / total).toInt()
                            if (percent != lastShown) {
                                lastShown = percent
                                onProgress("Copying back · $percent%")
                            }
                        }
                    }
                }
                if (target.length() != files[path]) throw IllegalStateException("$path didn't copy completely")
            }
            // Copied beside the game's home folder first, so a stopped copy leaves the current saves alone.
            for (path in wanted.keys.filter { it.startsWith("save/") }) copy(path, File(staging, path.removePrefix("save/")))
            staging.mkdirs()
            if (appTarget != null) {
                val partial = File(appTarget.parentFile, "${appTarget.name}.part").apply { deleteRecursively() }
                try {
                    for (path in wanted.keys.filter { it.startsWith("app/") }) copy(path, File(partial, path.removePrefix("app/")))
                    if (!partial.renameTo(appTarget)) throw IllegalStateException("${appTarget.name} couldn't be put in place")
                } finally {
                    partial.deleteRecursively()
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
        val kind: String?, val name: String?, val place: String?, val saved: Long?)

    private fun root(tree: Uri) = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))

    private fun displayName(context: Context, document: Uri): String? =
        context.contentResolver.query(document, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { if (it.moveToFirst()) it.getString(0) else null }

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
        }.orEmpty()
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
        val files = values.filter { it.first == "file" }.mapNotNull { (_, text) ->
            text.split(' ', limit = 2).takeIf { it.size == 2 }?.let { (size, path) -> size.toLongOrNull()?.let { path to it } }
        }.toMap()
        return Info(value("package") ?: return null, value("label") ?: "", value("versionCode")?.toLongOrNull() ?: 0, value("versionName") ?: "",
            value("installed")?.toLongOrNull(), files, value("kind"), value("name"), value("place"), value("saved")?.toLongOrNull())
    }
}
