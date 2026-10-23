package dev.braxten.gamebridge

/** Where a UIImageView puts its picture for each of Apple's UIViewContentMode values. */
object ContentMode {
    /**
     * The picture's left, top, right and bottom in a view of [viewWidth] x [viewHeight] points,
     * for a picture of [width] x [height] points shown with content mode [mode].
     */
    fun place(mode: Int, viewWidth: Float, viewHeight: Float, width: Float, height: Float): FloatArray {
        // Scale to fill (0) and redraw (3) stretch it over the whole view.
        if (mode == 0 || mode == 3 || width <= 0f || height <= 0f) return floatArrayOf(0f, 0f, viewWidth, viewHeight)
        // Aspect fit (1) and fill (2) scale it keeping its shape; the rest keep its own size.
        val ratio = when (mode) {
            1 -> minOf(viewWidth / width, viewHeight / height)
            2 -> maxOf(viewWidth / width, viewHeight / height)
            else -> 1f
        }
        val w = width * ratio
        val h = height * ratio
        // 4 center, 5 top, 6 bottom, 7 left, 8 right, 9 top left, 10 top right, 11 bottom left, 12 bottom right.
        val left = when (mode) {
            7, 9, 11 -> 0f
            8, 10, 12 -> viewWidth - w
            else -> (viewWidth - w) / 2
        }
        val top = when (mode) {
            5, 9, 10 -> 0f
            6, 11, 12 -> viewHeight - h
            else -> (viewHeight - h) / 2
        }
        return floatArrayOf(left, top, left + w, top + h)
    }
}
