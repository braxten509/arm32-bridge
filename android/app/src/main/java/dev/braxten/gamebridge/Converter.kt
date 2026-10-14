package dev.braxten.gamebridge

import android.content.Context
import com.android.apksig.ApkSigner
import java.io.File
import java.security.KeyStore
import java.security.PrivateKey
import java.security.cert.X509Certificate
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
 * Converts a game with apk-convert (the same program tools/convert.sh uses), signs it with the
 * PC's debug key, and installs it. A bundle's pieces are all signed and installed together, and
 * its game data (OBB files) copied where the game looks for it. A bundle that already runs here
 * is installed as it is.
 */
class Converter(private val context: Context, private val installer: Installer = RootInstaller) {
    private val libraries = File(context.applicationInfo.nativeLibraryDir)

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

    private fun convert(game: Game, work: File, report: (Progress) -> Unit): Outcome {
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
        if (code != 0) return Outcome.Failed(error ?: "The converter stopped (code $code).")

        report(Progress(75, "Signing"))
        val pieces = if (game.isBundle) unsigned.listFiles { file -> file.name.endsWith(".apk") }!!.sorted() else listOf(unsigned)
        // Numbered names: piece names come from the bundle and shouldn't reach a shell.
        val signed = pieces.mapIndexed { index, piece ->
            File(work, "signed-$index.apk").also { sign(piece, it); piece.delete() }
        }

        report(Progress(88, "Installing"))
        installer.install(signed)?.let { return Outcome.Failed(it) }
        val gameData = File(unsigned, "obb")
        if (gameData.isDirectory) {
            report(Progress(95, "Copying game data"))
            installer.copyGameData(gameData)?.let { return Outcome.Failed(it) }
        }
        report(Progress(100, "Installed"))
        return Outcome.Installed(missing)
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
        installer.install(pieces)?.let { return Outcome.Failed(it) }
        if (gameData.isDirectory) {
            report(Progress(95, "Copying game data"))
            installer.copyGameData(gameData)?.let { return Outcome.Failed(it) }
        }
        report(Progress(100, "Installed"))
        return Outcome.Installed(emptyList())
    }

    companion object {
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

    /** Signs with the debug key tools/build-app.sh copied from the PC, so PC and phone builds replace each other. */
    private fun sign(input: File, output: File) {
        val password = "android".toCharArray()
        val store = KeyStore.getInstance("PKCS12")
        context.assets.open("debug.keystore").use { store.load(it, password) }
        val key = store.getKey("androiddebugkey", password) as PrivateKey
        val certificates = store.getCertificateChain("androiddebugkey").map { it as X509Certificate }
        val signer = ApkSigner.SignerConfig.Builder("CERT", key, certificates).build()
        ApkSigner.Builder(listOf(signer))
            .setInputApk(input)
            .setOutputApk(output)
            .setV1SigningEnabled(true)
            .setV2SigningEnabled(true)
            .build()
            .sign()
    }
}
