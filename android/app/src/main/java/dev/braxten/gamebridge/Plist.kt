package dev.braxten.gamebridge

import android.util.Xml
import java.io.File
import java.nio.ByteBuffer
import org.xmlpull.v1.XmlPullParser

/**
 * Reads Apple property lists (an iPhone game's Info.plist), XML or binary. Dictionaries come
 * back as maps, arrays as lists, strings and numbers as themselves; dates and data as null.
 */
object Plist {
    fun read(file: File): Any? {
        val bytes = file.readBytes()
        return if (bytes.size >= 8 && String(bytes, 0, 8, Charsets.ISO_8859_1) == "bplist00") Binary(bytes).top() else xml(bytes)
    }

    private fun xml(bytes: ByteArray): Any? {
        val parser = Xml.newPullParser()
        parser.setInput(bytes.inputStream(), null)
        while (parser.next() != XmlPullParser.END_DOCUMENT) {
            if (parser.eventType == XmlPullParser.START_TAG && parser.name != "plist") return value(parser)
        }
        return null
    }

    /** The value whose start tag the parser is on; leaves it on the matching end tag. */
    private fun value(parser: XmlPullParser): Any? = when (parser.name) {
        "dict" -> {
            val map = LinkedHashMap<String, Any?>()
            var key: String? = null
            while (parser.nextTag() == XmlPullParser.START_TAG) {
                if (parser.name == "key") key = parser.nextText() else map[key ?: ""] = value(parser)
            }
            map
        }
        "array" -> {
            val list = ArrayList<Any?>()
            while (parser.nextTag() == XmlPullParser.START_TAG) list.add(value(parser))
            list
        }
        "true", "false" -> (parser.name == "true").also { parser.nextTag() }
        "string" -> parser.nextText()
        "integer" -> parser.nextText().trim().toLongOrNull()
        "real" -> parser.nextText().trim().toDoubleOrNull()
        else -> null.also { parser.nextText() }
    }

    /** Apple's binary format: objects found through an offset table, referred to by number. */
    private class Binary(bytes: ByteArray) {
        private val data = ByteBuffer.wrap(bytes)
        private val offsetSize: Int
        private val refSize: Int
        private val objects: Int
        private val topObject: Int
        private val tableAt: Int

        init {
            val trailer = bytes.size - 32
            offsetSize = bytes[trailer + 6].toInt()
            refSize = bytes[trailer + 7].toInt()
            objects = data.getLong(trailer + 8).toInt()
            topObject = data.getLong(trailer + 16).toInt()
            tableAt = data.getLong(trailer + 24).toInt()
        }

        fun top() = objectAt(topObject, 0)

        private fun number(at: Int, size: Int): Long {
            var value = 0L
            for (i in 0 until size) value = (value shl 8) or (data.get(at + i).toLong() and 0xFF)
            return value
        }

        private fun objectAt(index: Int, depth: Int): Any? {
            if (index !in 0 until objects || depth > 32) return null
            var at = number(tableAt + index * offsetSize, offsetSize).toInt()
            val marker = data.get(at).toInt() and 0xFF
            val kind = marker shr 4
            var count = marker and 0xF
            at++
            if (count == 0xF && kind in setOf(0x4, 0x5, 0x6, 0xA, 0xD)) {
                val sizeMarker = data.get(at).toInt() and 0xFF
                val size = 1 shl (sizeMarker and 0xF)
                count = number(at + 1, size).toInt()
                at += 1 + size
            }
            return when (kind) {
                0x0 -> when (marker) { 0x08 -> false; 0x09 -> true; else -> null }
                0x1 -> number(at, 1 shl (marker and 0xF))
                0x2 -> if (marker and 0xF == 3) data.getDouble(at) else data.getFloat(at).toDouble()
                0x5 -> String(ByteArray(count) { data.get(at + it) }, Charsets.US_ASCII)
                0x6 -> String(ByteArray(count * 2) { data.get(at + it) }, Charsets.UTF_16BE)
                0xA -> (0 until count).map { objectAt(number(at + it * refSize, refSize).toInt(), depth + 1) }
                0xD -> (0 until count).associate {
                    val key = objectAt(number(at + it * refSize, refSize).toInt(), depth + 1) as? String ?: ""
                    key to objectAt(number(at + (count + it) * refSize, refSize).toInt(), depth + 1)
                }
                else -> null
            }
        }
    }
}
