package dev.braxten.gamebridge

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

/**
 * Installs a converted game. Kept separate so the root method can be swapped for one that
 * works on phones without root later.
 */
interface Installer {
    /** Installs one APK, or a bundle's pieces together. Returns null on success, or a plain-language reason. */
    fun install(apks: List<File>): String?

    /** Copies a folder of game data (one folder per package inside) into /sdcard/Android/obb. */
    fun copyGameData(folder: File): String?
}

/**
 * Installs through root with the same flag the PC uses: Android refuses games made for very
 * old Android versions unless the installer asks it not to.
 */
object RootInstaller : Installer {
    override fun install(apks: List<File>): String? {
        // Android's installer can't read this app's private files, so they go through /data/local/tmp.
        val staged = apks.indices.map { "/data/local/tmp/gamebridge-$it.apk" }
        val removeStaged = "rm -f ${staged.joinToString(" ") { "'$it'" }}"
        val copy = Root.run(apks.zip(staged).joinToString(" && ") { (apk, to) -> "cp '${apk.absolutePath}' '$to' && chmod 644 '$to'" })
        if (copy.code != 0) {
            Root.run(removeStaged)
            return "Couldn't hand the game to Android: ${copy.output}"
        }
        // Given several files, pm installs them as one app: the base and its pieces.
        val result = Root.run("pm install -r --bypass-low-target-sdk-block ${staged.joinToString(" ") { "'$it'" }}; $removeStaged")
        return when {
            result.output.contains("Success") -> null
            result.output.contains("UPDATE_INCOMPATIBLE") ->
                "This game is already installed from somewhere else. Uninstall it first " +
                    "(that deletes its saved progress), then try again."
            else -> "Android didn't install it: ${result.output.lines().lastOrNull().orEmpty()}"
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
}
