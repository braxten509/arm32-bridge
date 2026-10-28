package dev.braxten.gamebridge

import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.util.zip.CRC32
import java.util.zip.Deflater
import java.util.zip.Inflater

/**
 * Apple's "crushed" PNGs (Xcode's pngcrush -iphone), which most iPhone games ship their icons as:
 * an extra CgBI chunk first, image data deflated without the zlib header, and pixels stored as
 * blue-green-red-alpha with the colors already multiplied by alpha. Android can't read them.
 */
object ApplePng {
    private val SIGNATURE = byteArrayOf(0x89.toByte(), 'P'.code.toByte(), 'N'.code.toByte(), 'G'.code.toByte(), 13, 10, 26, 10)

    /** A game's picture file as Android can read it: crushed PNGs uncrushed, other files as they are. */
    fun readable(file: File): ByteArray = file.readBytes().let { uncrush(it) ?: it }

    /** A normal PNG made from a crushed one; null when it isn't crushed or isn't the 8-bit RGBA kind Xcode makes. */
    fun uncrush(png: ByteArray): ByteArray? = runCatching {
        if (png.size < 8 || !png.copyOfRange(0, 8).contentEquals(SIGNATURE)) return null
        val input = ByteBuffer.wrap(png, 8, png.size - 8)
        var crushed = false
        var header: ByteArray? = null
        val data = ByteArrayOutputStream()
        while (input.remaining() >= 12) {
            val length = input.int
            val kind = String(ByteArray(4).also(input::get), Charsets.US_ASCII)
            val body = ByteArray(length).also(input::get)
            input.int // CRC
            when (kind) {
                "CgBI" -> crushed = true
                "IHDR" -> header = body
                "IDAT" -> data.write(body)
                "IEND" -> break
            }
        }
        val head = header ?: return null
        val ihdr = ByteBuffer.wrap(head)
        val width = ihdr.int
        val height = ihdr.int
        // 8 bits, color type 6 (RGBA), no interlacing: the only form Xcode writes for icons.
        if (!crushed || head[8] != 8.toByte() || head[9] != 6.toByte() || head[12] != 0.toByte()) return null
        if (width <= 0 || height <= 0 || width > 4096 || height > 4096) return null

        val stride = width * 4
        val raw = ByteArray((stride + 1) * height)
        Inflater(true).run {
            setInput(data.toByteArray())
            var done = 0
            while (done < raw.size && !finished()) {
                val n = inflate(raw, done, raw.size - done)
                if (n == 0 && (needsInput() || needsDictionary())) break
                done += n
            }
            end()
            if (done < raw.size) return null
        }

        // Undo the row filters (each predicts a byte from the same channel left, above and
        // above-left), then swap blue and red and divide the colors by alpha again.
        val out = ByteArray((stride + 1) * height)
        val previous = ByteArray(stride)
        val row = ByteArray(stride)
        for (y in 0 until height) {
            val start = y * (stride + 1)
            val filter = raw[start].toInt()
            for (i in 0 until stride) {
                val left = if (i >= 4) row[i - 4].toInt() and 0xFF else 0
                val above = previous[i].toInt() and 0xFF
                val aboveLeft = if (i >= 4) previous[i - 4].toInt() and 0xFF else 0
                val guess = when (filter) {
                    0 -> 0
                    1 -> left
                    2 -> above
                    3 -> (left + above) / 2
                    4 -> paeth(left, above, aboveLeft)
                    else -> return null
                }
                row[i] = ((raw[start + 1 + i].toInt() and 0xFF) + guess).toByte()
            }
            row.copyInto(previous)
            out[start] = 0
            for (x in 0 until width) {
                val p = x * 4
                val alpha = row[p + 3].toInt() and 0xFF
                fun straight(value: Byte): Byte {
                    val c = value.toInt() and 0xFF
                    return if (alpha == 0 || alpha == 255) value else minOf(255, (c * 255 + alpha / 2) / alpha).toByte()
                }
                out[start + 1 + p] = straight(row[p + 2])
                out[start + 2 + p] = straight(row[p + 1])
                out[start + 3 + p] = straight(row[p])
                out[start + 4 + p] = row[p + 3]
            }
        }

        val result = ByteArrayOutputStream()
        result.write(SIGNATURE)
        result.chunk("IHDR", head)
        val deflater = Deflater(Deflater.BEST_SPEED)
        deflater.setInput(out)
        deflater.finish()
        val compressed = ByteArrayOutputStream()
        val buffer = ByteArray(64 * 1024)
        while (!deflater.finished()) compressed.write(buffer, 0, deflater.deflate(buffer))
        deflater.end()
        result.chunk("IDAT", compressed.toByteArray())
        result.chunk("IEND", ByteArray(0))
        result.toByteArray()
    }.getOrNull()

    private fun paeth(a: Int, b: Int, c: Int): Int {
        val p = a + b - c
        val pa = Math.abs(p - a)
        val pb = Math.abs(p - b)
        val pc = Math.abs(p - c)
        return if (pa <= pb && pa <= pc) a else if (pb <= pc) b else c
    }

    private fun ByteArrayOutputStream.chunk(kind: String, body: ByteArray) {
        val name = kind.toByteArray(Charsets.US_ASCII)
        write(ByteBuffer.allocate(4).putInt(body.size).array())
        write(name)
        write(body)
        val crc = CRC32().apply { update(name); update(body) }
        write(ByteBuffer.allocate(4).putInt(crc.value.toInt()).array())
    }
}
