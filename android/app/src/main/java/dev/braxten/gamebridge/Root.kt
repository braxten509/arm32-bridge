package dev.braxten.gamebridge

import android.content.Context
import java.io.File

/** Runs commands as root through Magisk's `su`. */
object Root {
    class Result(val code: Int, val output: String)

    fun run(command: String): Result {
        val process = ProcessBuilder("su", "-c", command).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        return Result(process.waitFor(), output.trim())
    }

    fun available(): Boolean = runCatching { run("id").output.contains("uid=0") }.getOrDefault(false)

    /** Lets this app read every file in shared storage (the Downloads folder), without a trip to Settings. */
    fun grantFileAccess(packageName: String): Boolean =
        run("appops set $packageName MANAGE_EXTERNAL_STORAGE allow").code == 0

    /** Uninstalls a game; Android deletes its saved progress and game data with it. Null on success, else a plain reason. */
    fun uninstall(packageName: String): String? {
        if (!Regex("[A-Za-z0-9_.]+").matches(packageName)) return "That game has an odd name, so it wasn't removed."
        val result = run("pm uninstall '$packageName'")
        return if (result.output.contains("Success")) null
        else "Android didn't remove it: ${result.output.lines().lastOrNull().orEmpty()}"
    }

    /** Deletes a game file from shared storage. Null on success, else a plain reason. */
    fun delete(file: File): String? {
        if (file.delete() || !file.exists()) return null
        // The file access grant can lag right after the app starts; root can always delete it.
        val result = run("rm -f '${file.absolutePath.replace("'", "'\\''")}'")
        return if (result.code == 0) null else "Couldn't delete ${file.name}: ${result.output}"
    }
}

/** How an install ended. */
sealed class InstallResult {
    object Done : InstallResult()

    /** Android refuses games made for older versions than `sdk` (only without root). */
    class NeedsTarget(val sdk: Int) : InstallResult()

    class Failed(val reason: String) : InstallResult()
}

/** Installs, removes and copies game data: through root (RootInstaller) or Android's own installer (SessionInstaller). */
interface Installer {
    /** Whether games made for very old Android versions install as they are (root skips Android's age check). */
    val keepsOldTargets: Boolean

    /** Installs one APK, or a bundle's pieces together. */
    fun install(apks: List<File>): InstallResult

    /** Copies a folder of game data (one folder per package inside) into /sdcard/Android/obb. Null on success, else a plain reason. */
    fun copyGameData(folder: File): String?

    /** Uninstalls a game; Android deletes its saved progress and game data with it. Null on success, else a plain reason. */
    fun uninstall(packageName: String): String?

    /** Deletes a game file from shared storage. Null on success, else a plain reason. */
    fun delete(file: File): String?
}

/** Message shown when the game is installed with another key (from the Play Store or another phone's Game Bridge). */
const val OTHER_KEY = "This game is already installed from somewhere else. Uninstall it first " +
    "(that deletes its saved progress), then try again."

/**
 * Installs through root with the same flag the PC uses: Android refuses games made for very
 * old Android versions unless the installer asks it not to.
 */
object RootInstaller : Installer {
    override val keepsOldTargets = true

    override fun install(apks: List<File>): InstallResult {
        // Android's installer can't read this app's private files, so they go through /data/local/tmp.
        val staged = apks.indices.map { "/data/local/tmp/gamebridge-$it.apk" }
        val removeStaged = "rm -f ${staged.joinToString(" ") { "'$it'" }}"
        val copy = Root.run(apks.zip(staged).joinToString(" && ") { (apk, to) -> "cp '${apk.absolutePath}' '$to' && chmod 644 '$to'" })
        if (copy.code != 0) {
            Root.run(removeStaged)
            return InstallResult.Failed("Couldn't hand the game to Android: ${copy.output}")
        }
        // Given several files, pm installs them as one app: the base and its pieces.
        val result = Root.run("pm install -r --bypass-low-target-sdk-block ${staged.joinToString(" ") { "'$it'" }}; $removeStaged")
        return when {
            result.output.contains("Success") -> InstallResult.Done
            result.output.contains("UPDATE_INCOMPATIBLE") -> InstallResult.Failed(OTHER_KEY)
            else -> InstallResult.Failed("Android didn't install it: ${result.output.lines().lastOrNull().orEmpty()}")
        }
    }

    override fun copyGameData(folder: File): String? {
        // One folder per app; the names come from the bundle, so only plain app names reach the shell.
        val packages = folder.listFiles()!!.map { it.name }
        if (packages.any { !Regex("[A-Za-z0-9_.]+").matches(it) }) return "Installed, but the game data has an odd folder name, so it wasn't copied."
        // Root's copies come out readable by root only; the game reads them as itself, so open them up as adb push would.
        val opened = packages.joinToString(" ") { "'/sdcard/Android/obb/$it'" }
        val result = Root.run("mkdir -p /sdcard/Android/obb && cp -r '${folder.absolutePath}/.' /sdcard/Android/obb/ && chmod -R a+rX $opened")
        return if (result.code == 0) null else "Installed, but the game data couldn't be copied: ${result.output}"
    }

    override fun uninstall(packageName: String): String? = Root.uninstall(packageName)

    override fun delete(file: File): String? = Root.delete(file)
}

/**
 * Root when Magisk allows it, else Android's own installer. The phone setting
 * debug.gamebridge.noroot=1 forces the no-root path on a rooted phone, for testing it.
 * Asks `su`, so never on the main thread.
 */
object Installers {
    @Volatile
    private var chosen: Installer? = null

    fun current(context: Context): Installer = chosen ?: pick(context).also { chosen = it }

    private fun pick(context: Context): Installer =
        if (!forcedNoRoot() && Root.available()) RootInstaller else SessionInstaller(context)

    private fun forcedNoRoot(): Boolean = runCatching {
        val process = ProcessBuilder("getprop", "debug.gamebridge.noroot").start()
        process.inputStream.bufferedReader().readText().trim().also { process.waitFor() } == "1"
    }.getOrDefault(false)
}
