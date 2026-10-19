package dev.braxten.gamebridge

import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.zip.CRC32
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream

/**
 * Each iPhone game's own app: a tiny app with the game's name and icon, in the app list and on
 * the home screen like any other. It only opens the game in Game Bridge (OpenIphoneGame), which
 * runs it from its folder, so the game's saves and backups stay in Game Bridge.
 *
 * The app is the template in assets/iphone-launcher.apk (tools/build-iphone-launcher.sh) with its
 * placeholders replaced: package name, name, game folder, a stamp of all three plus the icon (so
 * Game Bridge can tell an app is up to date), and the icon picture. Game Bridge signs it with the
 * key it signs converted games with, and installs it; Android asks once for each new game, and
 * updates (a new name) go through without asking.
 */
object IphoneApps {
    private const val PREFIX = "dev.braxten.gamebridge.iphone."
    private const val PREFERENCES = "iphone-apps"
    private const val DECLINED = "declined"

    /** The app's package name: Game Bridge's, then the game's own id (com.chairentertainment.IB2). */
    fun packageOf(game: Game): String {
        val parts = game.packageName.lowercase().split('.', '-', ' ').filter { it.isNotEmpty() }.map { part ->
            val clean = part.map { if (it in 'a'..'z' || it in '0'..'9' || it == '_') it else '_' }.joinToString("")
            if (clean.first() in 'a'..'z') clean else "g$clean"
        }
        return PREFIX + parts.ifEmpty { listOf("game") }.joinToString(".")
    }

    fun isInstalled(context: Context, game: Game) = Games.isInstalled(context, packageOf(game))

    /** The user said no to Android's install question for this game: not asked again by itself. */
    private fun declined(context: Context): Set<String> =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getStringSet(DECLINED, emptySet()).orEmpty()

    private fun setDeclined(context: Context, packageName: String, value: Boolean) {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val set = declined(context).toMutableSet().apply { if (value) add(packageName) else remove(packageName) }
        preferences.edit().putStringSet(DECLINED, set).apply()
    }

    /**
     * Makes or updates the app of every iPhone game in `games` that has none or an outdated one
     * (one at a time; Android may ask for each), except those the user said no to. Runs off the
     * main thread; returns the problems to show.
     */
    @Synchronized
    fun update(context: Context, games: List<Game>): List<String> {
        val skip = declined(context)
        return games.filter { it.action == Action.PlayIphone && it.file != null && packageOf(it) !in skip }
            .mapNotNull { install(context, it, askedFor = false) }
    }

    /** Installs `game`'s app now (the card's "Install app"); null when done, else the reason. */
    @Synchronized
    fun install(context: Context, game: Game, askedFor: Boolean = true): String? {
        val packageName = packageOf(game)
        if (askedFor) setDeclined(context, packageName, false)
        val icon = iconPng(game.icon)
        val stamp = stamp(context, game, icon)
        if (installedStamp(context, packageName) == stamp) return null
        val work = File(context.cacheDir, "iphone-app").apply { deleteRecursively(); mkdirs() }
        try {
            val unsigned = File(work, "unsigned.apk")
            val signed = File(work, "app.apk")
            build(context, unsigned, packageName, game.label, game.file!!.path, stamp, icon)
            Signing.sign(context, unsigned, signed)
            return when (val result = Installer(context).install(listOf(signed))) {
                InstallResult.Done -> null
                is InstallResult.Failed -> {
                    if (result.reason == "The install was cancelled.") setDeclined(context, packageName, true)
                    "${game.label}: ${result.reason}"
                }
                else -> "${game.label}: Android didn't install its app."
            }
        } catch (failure: Exception) {
            return "${game.label}: couldn't make its app (${failure.message})"
        } finally {
            work.deleteRecursively()
        }
    }

    private fun installedStamp(context: Context, packageName: String): String? = runCatching {
        context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(PackageManager.GET_META_DATA.toLong()))
            .metaData?.getString("dev.braxten.gamebridge.stamp")
    }.getOrNull()

    /** Everything the app is made from, so a change to any of it makes a new app. */
    private fun stamp(context: Context, game: Game, icon: ByteArray?): String {
        val digest = MessageDigest.getInstance("SHA-256")
        for (part in listOf(packageOf(game), game.label, game.file?.path.orEmpty())) {
            digest.update(part.toByteArray())
            digest.update(0)
        }
        icon?.let(digest::update)
        context.assets.open(TEMPLATE).use { digest.update(it.readBytes()) }
        return digest.digest().take(12).joinToString("") { "%02x".format(it) }
    }

    private const val TEMPLATE = "iphone-launcher.apk"

    /** The icon as a 192-pixel PNG (the size of the largest launcher icons), or null to keep the template's. */
    private fun iconPng(icon: Bitmap?): ByteArray? {
        icon ?: return null
        val scaled = Bitmap.createScaledBitmap(icon, 192, 192, true)
        return ByteArrayOutputStream().also { scaled.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
    }

    /** The template with the placeholders replaced. resources.arsc stays uncompressed (the signer aligns it). */
    private fun build(context: Context, output: File, packageName: String, label: String, folder: String, stamp: String, icon: ByteArray?) {
        val replacements = mapOf(
            "dev.braxten.gamebridge.iphone.placeholder" to packageName,
            "GAME_BRIDGE_PLACEHOLDER_NAME" to label,
            "GAME_BRIDGE_PLACEHOLDER_FOLDER" to folder,
            "GAME_BRIDGE_PLACEHOLDER_STAMP" to stamp,
        )
        ZipInputStream(context.assets.open(TEMPLATE)).use { input ->
            ZipOutputStream(output.outputStream().buffered()).use { zip ->
                while (true) {
                    val entry = input.nextEntry ?: break
                    var bytes = input.readBytes()
                    when (entry.name) {
                        "AndroidManifest.xml" -> bytes = replaceStrings(bytes, replacements)
                        "res/mipmap/ic_launcher.png" -> if (icon != null) bytes = icon
                    }
                    val out = ZipEntry(entry.name).apply { time = entry.time }
                    if (entry.name == "resources.arsc") {
                        out.method = ZipEntry.STORED
                        out.size = bytes.size.toLong()
                        out.compressedSize = bytes.size.toLong()
                        out.crc = CRC32().apply { update(bytes) }.value
                    }
                    zip.putNextEntry(out)
                    zip.write(bytes)
                    zip.closeEntry()
                }
            }
        }
    }

    /**
     * Replaces whole strings in a compiled (binary) XML file's string pool. Everything else
     * refers to strings by number, so only the pool and the file's length change.
     */
    fun replaceStrings(xml: ByteArray, replacements: Map<String, String>): ByteArray {
        val file = ByteBuffer.wrap(xml).order(ByteOrder.LITTLE_ENDIAN)
        val poolStart = file.getShort(2).toInt() and 0xFFFF
        require(file.getShort(poolStart).toInt() == 0x0001) { "no string pool" }
        val headerSize = file.getShort(poolStart + 2).toInt() and 0xFFFF
        val poolSize = file.getInt(poolStart + 4)
        val count = file.getInt(poolStart + 8)
        val styles = file.getInt(poolStart + 12)
        val flags = file.getInt(poolStart + 16)
        val stringsStart = file.getInt(poolStart + 20)
        require(styles == 0) { "styled strings" }
        val utf8 = flags and 0x100 != 0
        val strings = (0 until count).map { index ->
            val at = poolStart + stringsStart + file.getInt(poolStart + headerSize + 4 * index)
            if (utf8) readUtf8(xml, at) else readUtf16(file, at)
        }.map { replacements[it] ?: it }

        val data = ByteArrayOutputStream()
        val offsets = strings.map { text ->
            val offset = data.size()
            data.write(if (utf8) encodeUtf8(text) else encodeUtf16(text))
            offset
        }
        while (data.size() % 4 != 0) data.write(0)
        val newStringsStart = headerSize + 4 * count
        val newPoolSize = newStringsStart + data.size()
        val pool = ByteBuffer.allocate(newPoolSize).order(ByteOrder.LITTLE_ENDIAN)
        pool.put(xml, poolStart, headerSize)
        pool.putInt(4, newPoolSize)
        pool.putInt(20, newStringsStart)
        pool.putInt(24, 0)
        pool.position(headerSize)
        offsets.forEach(pool::putInt)
        pool.put(data.toByteArray())

        val rest = xml.copyOfRange(poolStart + poolSize, xml.size)
        val result = ByteBuffer.allocate(poolStart + newPoolSize + rest.size).order(ByteOrder.LITTLE_ENDIAN)
        result.put(xml, 0, poolStart)
        result.put(pool.array())
        result.put(rest)
        result.putInt(4, result.capacity())
        return result.array()
    }

    private fun readUtf16(buffer: ByteBuffer, at: Int): String {
        var position = at
        var length = buffer.getShort(position).toInt() and 0xFFFF
        position += 2
        if (length and 0x8000 != 0) {
            length = ((length and 0x7FFF) shl 16) or (buffer.getShort(position).toInt() and 0xFFFF)
            position += 2
        }
        return String(CharArray(length) { buffer.getChar(position + 2 * it) })
    }

    private fun readUtf8(bytes: ByteArray, at: Int): String {
        var position = at
        // The length in UTF-16 units, then in bytes; each takes one byte, or two when the top bit is set.
        fun length(): Int {
            val first = bytes[position++].toInt() and 0xFF
            return if (first and 0x80 == 0) first else ((first and 0x7F) shl 8) or (bytes[position++].toInt() and 0xFF)
        }
        length()
        val size = length()
        return String(bytes, position, size, Charsets.UTF_8)
    }

    private fun encodeUtf16(text: String): ByteArray {
        val out = ByteBuffer.allocate(4 + 2 * text.length + 2).order(ByteOrder.LITTLE_ENDIAN)
        if (text.length > 0x7FFF) {
            out.putShort(((text.length shr 16) or 0x8000).toShort())
            out.putShort((text.length and 0xFFFF).toShort())
        } else {
            out.putShort(text.length.toShort())
        }
        text.forEach(out::putChar)
        out.putShort(0)
        return out.array().copyOf(out.position())
    }

    private fun encodeUtf8(text: String): ByteArray {
        val bytes = text.toByteArray(Charsets.UTF_8)
        val out = ByteArrayOutputStream()
        fun length(value: Int) {
            if (value > 0x7F) {
                out.write(((value shr 8) and 0x7F) or 0x80)
                out.write(value and 0xFF)
            } else {
                out.write(value)
            }
        }
        length(text.length)
        length(bytes.size)
        out.write(bytes)
        out.write(0)
        return out.toByteArray()
    }
}
