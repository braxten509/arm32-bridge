package dev.braxten.gamebridge

object IphoneScreen {
    enum class Orientation {
        PORTRAIT,
        PORTRAIT_UPSIDE_DOWN,
        LANDSCAPE_RIGHT,
        LANDSCAPE_LEFT,
    }

    enum class Request {
        FIXED_PORTRAIT,
        FIXED_PORTRAIT_UPSIDE_DOWN,
        SENSOR_PORTRAIT,
        FIXED_LANDSCAPE,
        FIXED_REVERSE_LANDSCAPE,
        SENSOR_LANDSCAPE,
    }

    data class Rules(
        val legacy: Boolean,
        val orientation: Orientation,
        val request: Request,
    ) {
        fun surfaceReady(width: Int, height: Int): Boolean = when (request) {
            Request.FIXED_PORTRAIT, Request.FIXED_PORTRAIT_UPSIDE_DOWN, Request.SENSOR_PORTRAIT -> height >= width
            Request.FIXED_LANDSCAPE, Request.FIXED_REVERSE_LANDSCAPE, Request.SENSOR_LANDSCAPE -> width >= height
        }
    }

    fun rules(info: Map<String, Any?>): Rules {
        val legacy = legacy(info)
        if (!legacy) return Rules(false, Orientation.LANDSCAPE_RIGHT, Request.SENSOR_LANDSCAPE)
        val initial = (info["UIInterfaceOrientation"] as? String)?.let(::orientation)
        val supported = listOf("UISupportedInterfaceOrientations~iphone", "UISupportedInterfaceOrientations")
            .asSequence()
            .mapNotNull { info[it] as? List<*> }
            .firstOrNull()
            ?.mapNotNull { (it as? String)?.let(::orientation) }
            .orEmpty()
            .ifEmpty { listOfNotNull(initial) }
            .ifEmpty { listOf(Orientation.LANDSCAPE_RIGHT) }
        val selected = initial?.takeIf { it in supported } ?: supported.first()
        val portraits = setOf(Orientation.PORTRAIT, Orientation.PORTRAIT_UPSIDE_DOWN)
        val landscapes = setOf(Orientation.LANDSCAPE_RIGHT, Orientation.LANDSCAPE_LEFT)
        val request = when {
            selected in portraits && supported.none { it in landscapes } ->
                when {
                    supported.contains(Orientation.PORTRAIT) && supported.contains(Orientation.PORTRAIT_UPSIDE_DOWN) ->
                        Request.SENSOR_PORTRAIT
                    supported.contains(Orientation.PORTRAIT_UPSIDE_DOWN) -> Request.FIXED_PORTRAIT_UPSIDE_DOWN
                    else -> Request.FIXED_PORTRAIT
                }
            selected in landscapes && supported.none { it in portraits } ->
                when {
                    supported.contains(Orientation.LANDSCAPE_RIGHT) && supported.contains(Orientation.LANDSCAPE_LEFT) ->
                        Request.SENSOR_LANDSCAPE
                    supported.contains(Orientation.LANDSCAPE_LEFT) -> Request.FIXED_REVERSE_LANDSCAPE
                    else -> Request.FIXED_LANDSCAPE
                }
            selected in portraits -> Request.SENSOR_PORTRAIT
            else -> Request.SENSOR_LANDSCAPE
        }
        return Rules(true, selected, request)
    }

    private fun legacy(info: Map<String, Any?>): Boolean {
        val sdk = (info["DTSDKName"] as? String)?.let(::major)
            ?: (info["DTPlatformVersion"] as? String)?.let(::major)
        return sdk != null && sdk <= 7
    }

    private fun major(text: String): Int? = text.removePrefix("iphoneos").substringBefore('.').toIntOrNull()

    private fun orientation(name: String): Orientation? = when (name) {
        "UIInterfaceOrientationPortrait" -> Orientation.PORTRAIT
        "UIInterfaceOrientationPortraitUpsideDown" -> Orientation.PORTRAIT_UPSIDE_DOWN
        "UIInterfaceOrientationLandscapeRight" -> Orientation.LANDSCAPE_RIGHT
        "UIInterfaceOrientationLandscapeLeft" -> Orientation.LANDSCAPE_LEFT
        else -> null
    }
}
