package dev.braxten.gamebridge

import android.content.Context
import java.io.File
import java.util.zip.ZipFile

/** Progress of one conversion: 0-100 and what is happening. */
class Progress(val percent: Int, val text: String)

/** How a conversion ended. */
sealed class Outcome {
    /** Installed. `missing` lists phone functions the bridge lacks (the game may fail if it uses them). */
    class Installed(val missing: List<String>) : Outcome()
    class Failed(val reason: String) : Outcome()
}

/**
 * Converts a game with apk-convert (the same program tools/convert.sh uses), signs it (Signing),
 * and installs it. A bundle's pieces are all signed and installed together, and
 * its game data (OBB files) copied where the game looks for it. A bundle that already runs here
 * is installed as it is.
 */
class Converter(private val context: Context, private val installer: Installer) {
    private val libraries = File(context.applicationInfo.nativeLibraryDir)
    private val settings = context.getSharedPreferences("converter", Context.MODE_PRIVATE)

    // The converter's own step lines, in order, and how far along each one is.
    private val stepPercent = mapOf(
        "Reading the game" to 5,
        "Checking which phone functions the game needs" to 15,
        "Swapping in the bridge" to 30,
        "Fixing known Java problems" to 55,
        "Copying game data" to 70,
    )

    fun convert(game: Game, report: (Progress) -> Unit): Outcome {
        val work = File(context.cacheDir, "convert").apply { deleteRecursively(); mkdirs() }
        try {
            return if (game.action == Action.Install) installAsIs(game, work, report) else convert(game, work, report)
        } catch (failure: Exception) {
            return Outcome.Failed(failure.message ?: failure.toString())
        } finally {
            work.deleteRecursively()
        }
    }

    /**
     * Android refuses games made for versions older than it supports. The oldest
     * version it takes is learned from its own refusal and remembered: later games are raised to
     * it up front, and a refused one is converted again with it, once.
     */
    private fun convert(game: Game, work: File, report: (Progress) -> Unit): Outcome {
        val known = settings.getInt(OLDEST_TARGET, 0).takeIf { it > 0 }
        val first = convert(game, work, report, known)
        if (first !is Attempt.Refused || (known != null && first.sdk <= known)) return first.outcome
        settings.edit().putInt(OLDEST_TARGET, first.sdk).apply()
        work.listFiles()?.forEach { it.deleteRecursively() }
        return convert(game, work, report, first.sdk).outcome
    }

    private sealed class Attempt(val outcome: Outcome) {
        class Ended(outcome: Outcome) : Attempt(outcome)
        class Refused(val sdk: Int) : Attempt(Outcome.Failed("Android refuses this game: it is made for an Android version older than $sdk."))
    }

    private fun convert(game: Game, work: File, report: (Progress) -> Unit, minimumTarget: Int?): Attempt {
        // A bundle converts into a folder of pieces (plus obb/ with its game data).
        val unsigned = File(work, if (game.isBundle) "unsigned" else "unsigned.apk")
        val missing = mutableListOf<String>()
        var error: String? = null
        report(Progress(0, "Starting"))
        val process = ProcessBuilder(
            File(libraries, "libapkconvert.so").path,
            game.file!!.path,
            unsigned.path,
            "--bridge", File(libraries, "libarm32_bridge.so").path,
            "--standin", File(libraries, "libstandin.so").path,
            "--work", work.path,
            *(minimumTarget?.let { arrayOf("--min-target-sdk", it.toString()) } ?: emptyArray()),
        ).redirectErrorStream(true).start()
        process.inputStream.bufferedReader().forEachLine { line ->
            val (kind, text) = line.split(' ', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
            when (kind) {
                "step" -> report(Progress(stepPercent[text] ?: 60, text))
                "missing" -> missing += text
                "error" -> error = text
            }
        }
        val code = process.waitFor()
        if (code != 0) return Attempt.Ended(Outcome.Failed(error ?: "The converter stopped (code $code)."))

        report(Progress(75, "Signing"))
        val pieces = if (game.isBundle) unsigned.listFiles { file -> file.name.endsWith(".apk") }!!.sorted() else listOf(unsigned)
        // Numbered names: piece names come from the bundle and shouldn't reach a shell.
        val signed = pieces.mapIndexed { index, piece ->
            File(work, "signed-$index.apk").also { Signing.sign(context, piece, it); piece.delete() }
        }

        report(Progress(88, "Installing"))
        when (val result = installer.install(signed)) {
            is InstallResult.Done -> {}
            is InstallResult.NeedsTarget -> return Attempt.Refused(result.sdk)
            is InstallResult.Failed -> return Attempt.Ended(Outcome.Failed(result.reason))
        }
        val gameData = File(unsigned, "obb")
        if (gameData.isDirectory) {
            report(Progress(95, "Copying game data"))
            installer.copyGameData(gameData)?.let { return Attempt.Ended(Outcome.Failed(it)) }
        }
        report(Progress(100, "Installed"))
        return Attempt.Ended(Outcome.Installed(missing))
    }

    /**
     * A bundle that already runs on this phone: its pieces (keeping their own signature) installed
     * together and its game data copied, the way the site's own installer app would.
     */
    private fun installAsIs(game: Game, work: File, report: (Progress) -> Unit): Outcome {
        report(Progress(0, "Unpacking"))
        val pieces = mutableListOf<File>()
        val gameData = File(work, "obb")
        ZipFile(game.file!!).use { zip ->
            val entries = zip.entries().toList().filter { !it.isDirectory }
            val apks = entries.filter { it.name.endsWith(".apk", ignoreCase = true) }
            // bundletool's .apks also holds whole-app copies for old phones next to the pieces.
            val hasSplits = apks.any { !it.name.startsWith("standalones/") }
            val chosen = apks.filter { !hasSplits || !it.name.startsWith("standalones/") }
            val data = entries.filter { it.name.startsWith("Android/obb/") }
            val total = (chosen + data).sumOf { it.size }.coerceAtLeast(1)
            var done = 0L
            fun copy(entry: java.util.zip.ZipEntry, to: File) {
                to.parentFile!!.mkdirs()
                zip.getInputStream(entry).use { input -> to.outputStream().use { input.copyTo(it) } }
                done += entry.size
                report(Progress((done * 80 / total).toInt(), "Unpacking"))
            }
            for ((index, entry) in chosen.withIndex()) {
                pieces += File(work, "piece-$index.apk").also { copy(entry, it) }
            }
            for (entry in data) {
                val relative = entry.name.removePrefix("Android/obb/")
                if (relative.split('/').any { it.isEmpty() || it == ".." }) return Outcome.Failed("The bundle has a game data file with an unsafe path.")
                copy(entry, File(gameData, relative))
            }
        }
        if (pieces.isEmpty()) return Outcome.Failed("The bundle has no APKs in it.")

        report(Progress(85, "Installing"))
        when (val result = installer.install(pieces)) {
            is InstallResult.Done -> {}
            // Its own signature can't survive a change, so it can't be raised the way a conversion is.
            is InstallResult.NeedsTarget -> return Outcome.Failed("Android refuses this game: it is made for an Android version older than ${result.sdk}.")
            is InstallResult.Failed -> return Outcome.Failed(result.reason)
        }
        if (gameData.isDirectory) {
            report(Progress(95, "Copying game data"))
            installer.copyGameData(gameData)?.let { return Outcome.Failed(it) }
        }
        report(Progress(100, "Installed"))
        return Outcome.Installed(emptyList())
    }

    companion object {
        private const val OLDEST_TARGET = "oldest_target_sdk"

        /**
         * apk-convert --inspect: its `kind` and `bits` lines (bits is 32, 64 or none), and for a
         * bundle its base APK copied to `base`. Empty when the file can't be read.
         */
        fun inspect(context: Context, file: File, base: File, work: File): Map<String, String> {
            val converter = File(context.applicationInfo.nativeLibraryDir, "libapkconvert.so")
            val process = ProcessBuilder(converter.path, "--inspect", file.path, "--base", base.path, "--work", work.path)
                .redirectErrorStream(true).start()
            val lines = process.inputStream.bufferedReader().readLines()
            if (process.waitFor() != 0) return emptyMap()
            return lines.mapNotNull { line -> line.split(' ', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } }.toMap()
        }
    }
}
