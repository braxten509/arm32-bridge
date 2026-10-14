package dev.braxten.gamebridge

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private val Background = Color(0xFF14161C)
private val CardColor = Color(0xFF1F2230)
private val Accent = Color(0xFF5B7CFA)
private val AccentText = Color(0xFF9FB2FF)
private val Muted = Color(0xFF9AA0AD)
private val Track = Color(0xFF2B2F3F)
private val ErrorText = Color(0xFFFF8A80)

/** What a game's card shows. */
sealed class CardState {
    object Ready : CardState()
    class Working(val progress: Progress) : CardState()
    object Done : CardState()
    class Failed(val reason: String) : CardState()
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            MaterialTheme(colorScheme = darkColorScheme(primary = Accent, background = Background, surface = CardColor)) {
                Screen()
            }
        }
    }

    @Composable
    private fun Screen() {
        val scope = rememberCoroutineScope()
        var games by remember { mutableStateOf<List<Game>?>(null) }
        var problem by remember { mutableStateOf<String?>(null) }
        // What happened to the last game picked with "Add a game" (null: nothing to say).
        var addNote by remember { mutableStateOf<String?>(null) }
        val states = Conversions.states
        val busy = Conversions.busy

        fun refresh() {
            scope.launch {
                val result = withContext(Dispatchers.IO) {
                    if (!Root.available()) return@withContext null to
                        "Game Bridge needs root (Magisk) to read Downloads and install games. Allow it in Magisk, then reopen."
                    Root.grantFileAccess(packageName)
                    val found = Games.scan(this@MainActivity)
                    for (game in found) {
                        val state = states[game.packageName]
                        if (state !is CardState.Working && state !is CardState.Failed) {
                            states[game.packageName] =
                                if (Games.isConverted(this@MainActivity, game.packageName)) CardState.Done else CardState.Ready
                        }
                    }
                    found to null
                }
                games = result.first
                problem = result.second
            }
        }

        fun convert(game: Game) = Conversions.convert(this@MainActivity, game)

        val pickGame = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri == null) return@rememberLauncherForActivityResult
            addNote = "Adding…"
            scope.launch {
                addNote = withContext(Dispatchers.IO) { Games.add(this@MainActivity, uri) }
                refresh()
            }
        }

        LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { refresh() }
        LaunchedEffect(Unit) { refresh() }

        Column(Modifier.fillMaxSize().background(Background).safeDrawingPadding()) {
            Text(
                "Game Bridge",
                color = Color(0xFFE8E8EE),
                fontSize = 22.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(start = 18.dp, top = 22.dp, bottom = 4.dp),
            )
            Text(
                "32-bit games",
                color = Muted,
                fontSize = 13.sp,
                modifier = Modifier.padding(start = 18.dp, bottom = 14.dp),
            )
            if (problem == null) {
                AddButton(enabled = addNote != "Adding…") {
                    // Some file apps don't label APKs as apps, so plain files are offered too.
                    pickGame.launch(arrayOf("application/vnd.android.package-archive", "application/octet-stream"))
                }
                addNote?.let { Message(it) }
            }
            when {
                problem != null -> Message(problem!!)
                games == null -> Message("Looking for games…")
                games!!.isEmpty() -> Message("No 32-bit games yet. Download a game's APK, or add one from anywhere above.")
                else -> LazyVerticalGrid(
                    columns = GridCells.Fixed(2),
                    contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 24.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(games!!, key = { it.file.path }) { game ->
                        GameCard(game, states[game.packageName] ?: CardState.Ready, busy, onConvert = { convert(game) }, onOpen = {
                            packageManager.getLaunchIntentForPackage(game.packageName)?.let(::startActivity)
                        })
                    }
                }
            }
        }
    }

    /** The dashed "Add a game from anywhere…" button above the games. */
    @Composable
    private fun AddButton(enabled: Boolean, onClick: () -> Unit) {
        Box(
            Modifier.padding(start = 12.dp, end = 12.dp, bottom = 14.dp).fillMaxWidth().height(44.dp)
                .clip(RoundedCornerShape(14.dp))
                .drawBehind {
                    val width = 1.5.dp.toPx()
                    drawRoundRect(
                        color = Accent,
                        topLeft = androidx.compose.ui.geometry.Offset(width / 2, width / 2),
                        size = androidx.compose.ui.geometry.Size(size.width - width, size.height - width),
                        cornerRadius = CornerRadius(14.dp.toPx() - width / 2),
                        style = Stroke(width, pathEffect = PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(), 3.dp.toPx()))),
                    )
                }
                .clickable(enabled = enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            Text("+ Add a game from anywhere…", color = if (enabled) AccentText else Muted, fontSize = 14.sp)
        }
    }

    @Composable
    private fun Message(text: String) {
        Text(text, color = Muted, fontSize = 14.sp, modifier = Modifier.padding(horizontal = 18.dp, vertical = 8.dp))
    }

    @Composable
    private fun GameCard(game: Game, state: CardState, busy: Boolean, onConvert: () -> Unit, onOpen: () -> Unit) {
        Column(
            Modifier.clip(RoundedCornerShape(16.dp)).background(CardColor).padding(14.dp).fillMaxWidth(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Box(Modifier.size(72.dp).clip(RoundedCornerShape(18.dp)).background(Track)) {
                game.icon?.let { Image(it.asImageBitmap(), contentDescription = null, modifier = Modifier.fillMaxSize()) }
            }
            Spacer(Modifier.height(10.dp))
            Text(
                game.label,
                color = Color(0xFFE8E8EE),
                fontSize = 14.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
            )
            Text(game.place, color = Muted, fontSize = 11.sp, maxLines = 1, modifier = Modifier.padding(top = 3.dp))
            Spacer(Modifier.height(10.dp))
            // Every state uses the same 36dp slot, so cards keep their height while converting.
            Box(Modifier.fillMaxWidth().height(36.dp), contentAlignment = Alignment.Center) {
                when (state) {
                    CardState.Ready -> Button(
                        onClick = onConvert,
                        enabled = !busy,
                        colors = ButtonDefaults.buttonColors(containerColor = Accent),
                        contentPadding = PaddingValues(0.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) { Text("Convert", fontSize = 13.sp) }
                    is CardState.Working -> Column(Modifier.fillMaxWidth()) {
                        Text("${state.progress.percent}% · ${state.progress.text}", color = Muted, fontSize = 12.sp, maxLines = 1,
                            overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center, modifier = Modifier.fillMaxWidth())
                        Spacer(Modifier.height(8.dp))
                        LinearProgressIndicator(
                            progress = { state.progress.percent / 100f },
                            color = Accent,
                            trackColor = Track,
                            modifier = Modifier.fillMaxWidth().height(4.dp),
                        )
                    }
                    CardState.Done -> OutlinedButton(
                        onClick = onOpen,
                        contentPadding = PaddingValues(0.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) { Text("Open", color = AccentText, fontSize = 13.sp) }
                    is CardState.Failed -> OutlinedButton(
                        onClick = onConvert,
                        enabled = !busy,
                        contentPadding = PaddingValues(0.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) { Text("Try again", color = AccentText, fontSize = 13.sp) }
                }
            }
            // The same space on every card, so cards in a row stay the same height.
            Box(Modifier.padding(top = 6.dp).height(28.dp), contentAlignment = Alignment.Center) {
                if (state == CardState.Done) {
                    // For when the bridge has been updated: replaces the installed copy and keeps its saves.
                    Text(
                        "Convert again",
                        color = if (busy) Muted else AccentText,
                        fontSize = 12.sp,
                        modifier = Modifier.clip(RoundedCornerShape(8.dp))
                            .clickable(enabled = !busy, onClick = onConvert).padding(horizontal = 8.dp, vertical = 4.dp),
                    )
                }
            }
            val note = when (state) {
                is CardState.Failed -> state.reason
                else -> null
            }
            if (note != null) {
                Spacer(Modifier.height(8.dp))
                Text(note, color = if (state is CardState.Failed) ErrorText else Muted, fontSize = 11.sp,
                    textAlign = TextAlign.Center, maxLines = 4, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}
