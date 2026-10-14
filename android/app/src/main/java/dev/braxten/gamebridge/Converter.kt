package dev.braxten.gamebridge

import android.content.Context
import com.android.apksig.ApkSigner
import java.io.File
import java.security.KeyStore
import java.security.PrivateKey
import java.security.cert.X509Certificate

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
 * PC's debug key, and installs it.
 */
class Converter(private val context: Context, private val installer: Installer = RootInstaller) {
    private val libraries = File(context.applicationInfo.nativeLibraryDir)

    // The converter's own step lines, in order, and how far along each one is.
    private val stepPercent = mapOf(
        "Reading the game" to 5,
        "Checking which phone functions the game needs" to 15,
        "Swapping in the bridge" to 30,
        "Fixing known Java problems" to 55,
    )

    fun convert(game: Game, report: (Progress) -> Unit): Outcome {
        val work = File(context.cacheDir, "convert").apply { deleteRecursively(); mkdirs() }
        try {
            val unsigned = File(work, "unsigned.apk")
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
            val signed = File(work, "signed.apk")
            sign(unsigned, signed)
            unsigned.delete()

            report(Progress(88, "Installing"))
            installer.install(signed)?.let { return Outcome.Failed(it) }
            report(Progress(100, "Installed"))
            return Outcome.Installed(missing)
        } catch (failure: Exception) {
            return Outcome.Failed(failure.message ?: failure.toString())
        } finally {
            work.deleteRecursively()
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
