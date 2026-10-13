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
}

/**
 * Installs a converted game. Kept separate so the root method can be swapped for one that
 * works on phones without root later.
 */
fun interface Installer {
    /** Returns null on success, or a plain-language reason. */
    fun install(apk: File): String?
}

/**
 * Installs through root with the same flag the PC uses: Android refuses games made for very
 * old Android versions unless the installer asks it not to.
 */
object RootInstaller : Installer {
    override fun install(apk: File): String? {
        val staged = "/data/local/tmp/gamebridge-${apk.name}"
        val copy = Root.run("cp '${apk.absolutePath}' '$staged' && chmod 644 '$staged'")
        if (copy.code != 0) return "Couldn't hand the game to Android: ${copy.output}"
        val result = Root.run("pm install -r --bypass-low-target-sdk-block '$staged'; rm -f '$staged'")
        return when {
            result.output.contains("Success") -> null
            result.output.contains("UPDATE_INCOMPATIBLE") ->
                "This game is already installed from somewhere else. Uninstall it first " +
                    "(that deletes its saved progress), then convert again."
            else -> "Android didn't install it: ${result.output.lines().lastOrNull().orEmpty()}"
        }
    }
}
