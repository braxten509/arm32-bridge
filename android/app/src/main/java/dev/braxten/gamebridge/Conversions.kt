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

    /** True while a conversion runs (one at a time: they share a work folder), or while game files move. */
    var busy by mutableStateOf(false)
        private set

    /** True while game files move into the Game Bridge folder (a backup waits: it reads them). */
    var moving by mutableStateOf(false)
        private set

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** `replace`: remove a copy of the game signed by someone else (Android asks), then install this one. */
    fun convert(context: Context, game: Game, replace: Boolean = false) {
        if (busy || game.file == null) return
        busy = true
        val appContext = context.applicationContext
        scope.launch {
            val outcome = withContext(Dispatchers.IO) {
                Converter(appContext, Installer(appContext)).convert(game, replace) { progress ->
                    scope.launch { states[game.packageName] = CardState.Working(progress) }
                }
            }
            states[game.packageName] = when (outcome) {
                is Outcome.Installed -> CardState.Done
                is Outcome.Failed -> CardState.Failed(outcome.reason, replace)
                is Outcome.OtherCopy -> CardState.OtherCopy
            }
            busy = false
        }
    }

    /** Moves the games' files into the Game Bridge folder, then `onDone` with the first problem, or null. */
    fun move(context: Context, games: List<Game>, onDone: (String?) -> Unit) {
        if (busy || Backups.status != null) return
        busy = true
        moving = true
        val appContext = context.applicationContext
        scope.launch {
            val problem = withContext(Dispatchers.IO) { games.mapNotNull { Games.move(appContext, it) }.firstOrNull() }
            moving = false
            busy = false
            onDone(problem)
        }
    }
}
