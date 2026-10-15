package dev.braxten.gamebridge

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Conversion state for the whole app process, not one screen: turning the
 * phone rebuilds the screen, and a conversion running for the old screen must
 * stay tracked so a second one can't start over it.
 */
object Conversions {
    /** Each game's card, by package name. */
    val states = mutableStateMapOf<String, CardState>()

    /** True while a conversion runs (one at a time: they share a work folder). */
    var busy by mutableStateOf(false)
        private set

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    fun convert(context: Context, game: Game) {
        if (busy || game.file == null) return
        busy = true
        val appContext = context.applicationContext
        scope.launch {
            val outcome = withContext(Dispatchers.IO) {
                Converter(appContext, Installer(appContext)).convert(game) { progress ->
                    scope.launch { states[game.packageName] = CardState.Working(progress) }
                }
            }
            states[game.packageName] = when (outcome) {
                is Outcome.Installed -> CardState.Done
                is Outcome.Failed -> CardState.Failed(outcome.reason)
            }
            busy = false
        }
    }
}
