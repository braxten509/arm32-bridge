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
) {
    /** This version is installed, converted, with all its game data files in place: nothing to restore. */
    fun inPlace(context: Context): Boolean {
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
 * all match it is an unfinished backup.
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

    /** Backs up every converted game (and its original file when `originals`). Runs in the background; `status` follows it. */
    fun backUp(context: Context, games: List<Pair<String, File?>>, originals: Boolean, onDone: () -> Unit = {}) {
        if (status != null) return
        val appContext = context.applicationContext
        status = "Getting ready…"
        problem = null
        scope.launch {
            val failure = withContext(Dispatchers.IO) {
                runCatching { backUpAll(appContext, games, originals) }.exceptionOrNull()
            }
            problem = failure?.let { "The backup stopped: ${it.message ?: it.javaClass.simpleName}" }
            if (failure == null) preferences(appContext).edit().putLong(LAST, System.currentTimeMillis()).apply()
            status = null
            onDone()
        }
    }

    /** One file to save: its name in the game's folder, its size, and how to read it. */
    private class Item(val path: String, val size: Long, val open: () -> InputStream)

    private fun backUpAll(context: Context, games: List<Pair<String, File?>>, originals: Boolean) {
        val tree = folder(context) ?: throw IllegalStateException("pick a backup folder first")
        val top = child(context, tree, root(tree), TOP, directory = true)
        val packages = context.packageManager
        val plans = games.map { (packageName, original) ->
            val application = packages.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
            val items = mutableListOf<Item>()
            (listOf(File(application.sourceDir)) + application.splitSourceDirs.orEmpty().map(::File)).forEach { apk ->
                items += Item("apk/${apk.name}", apk.length()) { apk.inputStream() }
            }
            Games.obbFolder(packageName).listFiles().orEmpty().filter { it.isFile }.forEach { data ->
                items += Item("obb/${data.name}", data.length()) { data.inputStream() }
            }
            if (originals && original != null && original.isFile) items += Item("original/${original.name}", original.length()) { original.inputStream() }
            Triple(packageName, application, items)
        }
        val total = plans.sumOf { (_, _, items) -> items.sumOf { it.size } }.coerceAtLeast(1)
        var done = 0L
        for ((packageName, application, items) in plans) {
            val label = application.loadLabel(packages).toString()
            val info = packages.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
            val gameFolder = child(context, tree, top, packageName, directory = true)
            val savedInfo = readInfo(context, tree, gameFolder)
            val saved = savedInfo?.files.orEmpty()
            // A game installed again (converted again, updated) has new APKs even when their sizes match.
            val sameInstall = savedInfo?.installed == info.lastUpdateTime
            val present = listing(context, tree, gameFolder, "")
            // Already saved by the earlier, finished backup (info.txt exists only for finished ones):
            // same name and size (or a size the storage app doesn't tell), and for the APKs the same install.
            fun unchanged(item: Item) = saved[item.path] == item.size && present[item.path].let { it == item.size || it == -1L } &&
                (sameInstall || !item.path.startsWith("apk/"))
            // Changing files would leave the old info.txt calling a half-changed backup finished.
            if (items.any { !unchanged(it) }) find(context, tree, gameFolder, INFO)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
            for (item in items) {
                if (unchanged(item)) {
                    done += item.size
                    continue
                }
                val folderPath = item.path.substringBeforeLast('/')
                val parent = child(context, tree, gameFolder, folderPath, directory = true)
                val target = child(context, tree, parent, item.path.substringAfterLast('/'), directory = false)
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
            // Files of an earlier backup this one doesn't have (an old version's data file) go,
            // except original files when this backup leaves them out.
            val kept = items.map { it.path }.toSet()
            present.keys.filter { it !in kept && (originals || !it.startsWith("original/")) }.forEach { path ->
                find(context, tree, gameFolder, path)?.let { DocumentsContract.deleteDocument(context.contentResolver, it) }
            }
            val icon = runCatching { application.loadIcon(packages) }.getOrNull()
            Games.iconBitmap(icon)?.let { picture ->
                val target = child(context, tree, gameFolder, "icon.png", directory = false)
                context.contentResolver.openOutputStream(target, "wt")!!.use { picture.compress(Bitmap.CompressFormat.PNG, 100, it) }
            }
            // The sizes this backup wrote, and earlier original files kept as they are listed.
            val listed = items.associate { it.path to it.size } +
                listing(context, tree, gameFolder, "").filterKeys { it.startsWith("original/") && it !in kept }
                    .mapValues { (path, size) -> if (size >= 0) size else saved[path] ?: size }.filterValues { it >= 0 }
            val text = buildString {
                appendLine("package=$packageName")
                appendLine("label=$label")
                appendLine("versionCode=${info.longVersionCode}")
                appendLine("versionName=${info.versionName ?: ""}")
                appendLine("installed=${info.lastUpdateTime}")
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
        return children(context, tree, top).filter { it.directory }.mapNotNull { folder ->
            val info = readInfo(context, tree, folder.uri) ?: return@mapNotNull null
            val present = listing(context, tree, folder.uri, "")
            // Some storage apps don't tell sizes (-1): then the file being there has to do.
            if (info.files.isEmpty() || info.files.any { (path, size) -> present[path].let { it == null || (it >= 0 && it != size) } }) return@mapNotNull null
            val icon = find(context, tree, folder.uri, "icon.png")?.let { uri ->
                runCatching { context.contentResolver.openInputStream(uri)!!.use(BitmapFactory::decodeStream) }.getOrNull()
            }
            val data = info.files.filterKeys { it.startsWith("obb/") }.mapKeys { it.key.removePrefix("obb/") }
            SavedGame(info.packageName, info.label, info.versionCode, info.versionName, icon, info.files.values.sum(), folder.uri, data)
        }.sortedBy { it.label.lowercase() }
    }

    /** Installs a saved game, puts its data files back and its original file in Download (if not there). Null on success, else a plain reason. */
    fun restore(context: Context, game: SavedGame, onProgress: (String) -> Unit): String? {
        val tree = folder(context) ?: return "Pick the backup folder again."
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

    // Documents in the picked folder's tree.

    private class Entry(val uri: Uri, val name: String, val directory: Boolean, val size: Long)

    private class Info(val packageName: String, val label: String, val versionCode: Long, val versionName: String, val installed: Long?, val files: Map<String, Long>)

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

    /** The document at `path` ("obb/main.1.x.obb") below `parent`, if there. */
    private fun find(context: Context, tree: Uri, parent: Uri, path: String): Uri? =
        path.split('/').fold(parent as Uri?) { folder, name -> folder?.let { children(context, tree, it).firstOrNull { entry -> entry.name == name }?.uri } }

    /** The folder or file at `path` below `parent`, made when missing. */
    private fun child(context: Context, tree: Uri, parent: Uri, path: String, directory: Boolean): Uri =
        path.split('/').let { names ->
            names.foldIndexed(parent) { index, folder, name ->
                val isDirectory = directory || index < names.size - 1
                children(context, tree, folder).firstOrNull { it.name == name }?.uri
                    ?: (DocumentsContract.createDocument(context.contentResolver, folder,
                        if (isDirectory) Document.MIME_TYPE_DIR else "application/octet-stream", name)
                        ?: throw IllegalStateException("the backup folder refused to make $name")).also { made ->
                        // Some storage apps rename new files (adding an ending or " (1)"); the backup must find it by name later.
                        if (displayName(context, made) != name) {
                            runCatching { DocumentsContract.deleteDocument(context.contentResolver, made) }
                            throw IllegalStateException("the backup folder renamed $name, so it can't be used for backups")
                        }
                    }
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
            value("installed")?.toLongOrNull(), files)
    }
}
