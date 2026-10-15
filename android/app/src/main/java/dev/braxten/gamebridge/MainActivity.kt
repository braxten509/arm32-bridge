package dev.braxten.gamebridge

import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
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

/** What the setup screen shows: which of Android's two permissions the user has given (no-root phones only). */
private class Setup(val files: Boolean, val installs: Boolean)

class MainActivity : ComponentActivity() {
    companion object {
        /**
         * Whether "Install unknown apps" was on when this process started. Android opens other apps'
         * Android/obb folders (for game data) to an installer only at process start, so turning it
         * on later needs a restart.
         */
        private var installsAtStart: Boolean? = null
    }

    /** Starts the app again in a new process (Android's settings changes take effect then). */
    private fun restart() {
        startActivity(Intent.makeRestartActivityTask(ComponentName(this, MainActivity::class.java)))
        Runtime.getRuntime().exit(0)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (installsAtStart == null) installsAtStart = packageManager.canRequestPackageInstalls()
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
        // Shown instead of the games until both permissions are given.
        var setup by remember { mutableStateOf<Setup?>(null) }
        // True while the phone is being searched (the refresh button waits).
        var searching by remember { mutableStateOf(false) }
        val states = Conversions.states
        val busy = Conversions.busy

        fun refresh() {
            if (searching) return
            searching = true
            scope.launch {
                val result = withContext(Dispatchers.IO) {
                    if (Installers.current(this@MainActivity) is RootInstaller) {
                        Root.grantFileAccess(packageName)
                    } else {
                        val needed = Setup(Environment.isExternalStorageManager(), packageManager.canRequestPackageInstalls())
                        if (!needed.files || !needed.installs) return@withContext null to needed
                    }
                    val found = Games.scan(this@MainActivity)
                    for (game in found) {
                        val state = states[game.packageName]
                        if (state !is CardState.Working && state !is CardState.Failed) {
                            val installed = when (game.action) {
                                Action.Convert -> Games.isConverted(this@MainActivity, game.packageName)
                                Action.Install -> Games.isInstalled(this@MainActivity, game.packageName)
                            }
                            states[game.packageName] = if (installed) CardState.Done else CardState.Ready
                        }
                    }
                    found to null
                }
                result.first?.let { games = it }
                setup = result.second
                searching = false
                // Without root, Android/obb opens up only in a process started with the switch on.
                if (setup == null && installsAtStart == false && Installers.current(this@MainActivity) !is RootInstaller) restart()
            }
        }

        fun convert(game: Game) = Conversions.convert(this@MainActivity, game)

        // The game whose Remove dialog is showing.
        var removing by remember { mutableStateOf<Game?>(null) }

        /** Uninstalls the game and/or deletes its file, then searches again so the card updates or goes. */
        fun remove(game: Game, uninstall: Boolean, deleteFile: Boolean) {
            scope.launch {
                val problems = withContext(Dispatchers.IO) {
                    listOfNotNull(
                        if (uninstall) Installers.current(this@MainActivity).uninstall(game.packageName) else null,
                        if (deleteFile) game.file?.let(Installers.current(this@MainActivity)::delete) else null,
                    )
                }
                problems.firstOrNull()?.let { Toast.makeText(this@MainActivity, it, Toast.LENGTH_LONG).show() }
                states.remove(game.packageName)
                refresh()
            }
        }

        removing?.let { game ->
            RemoveDialog(game, installed = Games.isInstalled(this, game.packageName), onDismiss = { removing = null }) { uninstall, deleteFile ->
                removing = null
                remove(game, uninstall, deleteFile)
            }
        }

        // Searching the whole phone takes a moment, so only when the app opens and on the refresh button.
        LaunchedEffect(Unit) { refresh() }
        // Back from Android's settings: check the permissions again.
        LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
            if (setup != null) refresh()
            // An install or removal that finished converting while the app was in the background:
            // Android wouldn't show its confirmation then. A moment's wait lets an answer that is
            // already on its way arrive first.
            scope.launch {
                delay(800)
                Confirmation.waiting?.let { runCatching { startActivity(it) } }
            }
        }

        Column(Modifier.fillMaxSize().background(Background).safeDrawingPadding()) {
            Row(Modifier.fillMaxWidth().padding(end = 14.dp), verticalAlignment = Alignment.Bottom) {
                Text(
                    "Game Bridge",
                    color = Color(0xFFE8E8EE),
                    fontSize = 22.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f).padding(start = 18.dp, top = 22.dp, bottom = 4.dp),
                )
                RefreshButton(enabled = !searching && setup == null, onClick = ::refresh)
            }
            Text(
                "32-bit games and game bundles on this phone",
                color = Muted,
                fontSize = 13.sp,
                modifier = Modifier.padding(start = 18.dp, bottom = 14.dp),
            )
            when {
                setup != null -> SetupSteps(setup!!)
                games == null -> Message("Looking for games…")
                games!!.isEmpty() -> Message("No 32-bit games or game bundles on this phone. Download a game's APK, XAPK, APKM or APKS file, then tap ↻.")
                else -> LazyVerticalGrid(
                    columns = GridCells.Fixed(2),
                    contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 24.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(games!!, key = { it.file?.path ?: it.packageName }) { game ->
                        GameCard(game, states[game.packageName] ?: CardState.Ready, busy, onConvert = { convert(game) }, onRemove = { removing = game }, onOpen = {
                            packageManager.getLaunchIntentForPackage(game.packageName)?.let(::startActivity)
                        })
                    }
                }
            }
        }
    }

    /**
     * The two permissions a phone without root needs, each with a button to its page in Android's
     * settings. The screen checks again when the user comes back.
     */
    @Composable
    private fun SetupSteps(setup: Setup) {
        Column(Modifier.padding(horizontal = 12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(
                "Two quick steps before your first game",
                color = Color(0xFFE8E8EE),
                fontSize = 17.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(start = 6.dp, top = 4.dp),
            )
            SetupStep(1, "Find games", "Lets Game Bridge see game files in Downloads and your other folders.", setup.files) {
                startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:$packageName")))
            }
            SetupStep(2, "Install games", "Lets Game Bridge install the games it converts, and copy their game data.", setup.installs) {
                startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")))
            }
            Text(
                "Android asks you to confirm each game install.",
                color = Muted,
                fontSize = 12.sp,
                modifier = Modifier.padding(start = 6.dp),
            )
        }
    }

    @Composable
    private fun SetupStep(number: Int, title: String, text: String, done: Boolean, onAllow: () -> Unit) {
        Row(
            Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(CardColor).padding(14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.size(28.dp).clip(CircleShape).background(if (done) Track else Accent), contentAlignment = Alignment.Center) {
                Text("$number", color = if (done) Muted else Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            }
            Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                Text(title, color = Color(0xFFE8E8EE), fontSize = 15.sp)
                Text(text, color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 2.dp))
            }
            if (done) {
                Text("✓ Done", color = AccentText, fontSize = 13.sp)
            } else {
                Button(
                    onClick = onAllow,
                    colors = ButtonDefaults.buttonColors(containerColor = Accent),
                    contentPadding = PaddingValues(horizontal = 16.dp),
                    modifier = Modifier.height(36.dp),
                ) { Text("Allow", fontSize = 13.sp) }
            }
        }
    }

    /** The round "search the phone again" button next to the title; dimmed while a search runs. */
    @Composable
    private fun RefreshButton(enabled: Boolean, onClick: () -> Unit) {
        Box(
            Modifier.size(40.dp).clip(CircleShape).background(CardColor).clickable(enabled = enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            RefreshIcon(if (enabled) AccentText else Muted)
        }
    }

    /**
     * A circle arrow drawn by hand: a font's arrow sign sits off-center and is too thin. The
     * whole shape (ring plus arrowhead) is centered, so the arrowhead doesn't pull it up and right.
     */
    @Composable
    private fun RefreshIcon(color: Color) {
        Canvas(Modifier.size(24.dp)) {
            val stroke = 2.8.dp.toPx()
            val head = 5.5.dp.toPx()
            val radius = (size.minDimension - stroke) / 2 - 2.5.dp.toPx()
            val start = Math.toRadians(-60.0)
            val tip = Offset(center.x + radius * Math.cos(start).toFloat(), center.y + radius * Math.sin(start).toFloat())
            // The arrowhead points back along the ring from where it starts.
            val forward = Offset(Math.sin(start).toFloat(), -Math.cos(start).toFloat())
            val outward = Offset(Math.cos(start).toFloat(), Math.sin(start).toFloat())
            val corners = listOf(tip + forward * head, tip + outward * (head * 0.75f), tip - outward * (head * 0.75f))
            val xs = corners.map { it.x } + listOf(center.x - radius - stroke / 2, center.x + radius + stroke / 2)
            val ys = corners.map { it.y } + listOf(center.y - radius - stroke / 2, center.y + radius + stroke / 2)
            translate(center.x - (xs.min() + xs.max()) / 2, center.y - (ys.min() + ys.max()) / 2) {
                drawArc(
                    color, startAngle = -60f, sweepAngle = 290f, useCenter = false,
                    topLeft = Offset(center.x - radius, center.y - radius), size = Size(radius * 2, radius * 2),
                    style = Stroke(stroke, cap = StrokeCap.Round),
                )
                drawPath(Path().apply {
                    moveTo(corners[0].x, corners[0].y)
                    lineTo(corners[1].x, corners[1].y)
                    lineTo(corners[2].x, corners[2].y)
                    close()
                }, color)
            }
        }
    }

    @Composable
    private fun Message(text: String) {
        Text(text, color = Muted, fontSize = 14.sp, modifier = Modifier.padding(horizontal = 18.dp, vertical = 8.dp))
    }

    @Composable
    private fun GameCard(game: Game, state: CardState, busy: Boolean, onConvert: () -> Unit, onRemove: () -> Unit, onOpen: () -> Unit) {
        // Press and hold a card for its menu; a running conversion can't be removed under itself.
        var menu by remember { mutableStateOf(false) }
        Box {
            Column(
                Modifier.clip(RoundedCornerShape(16.dp)).background(CardColor)
                    .combinedClickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                        onClick = {},
                        onLongClick = { if (state !is CardState.Working) menu = true },
                    )
                    .padding(14.dp).fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) { CardContent(game, state, busy, onConvert, onOpen) }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }, containerColor = Track) {
                val close = { menu = false }
                if (state == CardState.Done) MenuItem("Open") { close(); onOpen() }
                if (game.file != null && !busy) when (state) {
                    CardState.Ready -> MenuItem(if (game.action == Action.Install) "Install" else "Convert") { close(); onConvert() }
                    CardState.Done -> if (game.action == Action.Convert) MenuItem("Convert again") { close(); onConvert() }
                    is CardState.Failed -> MenuItem("Try again") { close(); onConvert() }
                    is CardState.Working -> {}
                }
                val installed = Games.isInstalled(this@MainActivity, game.packageName)
                if (installed || game.file != null) MenuItem(if (installed) "Remove" else "Delete file", ErrorText) { close(); onRemove() }
            }
        }
    }

    @Composable
    private fun MenuItem(text: String, color: Color = Color(0xFFE8E8EE), onClick: () -> Unit) {
        DropdownMenuItem(text = { Text(text, color = color, fontSize = 14.sp) }, onClick = onClick)
    }

    @Composable
    private fun CardContent(game: Game, state: CardState, busy: Boolean, onConvert: () -> Unit, onOpen: () -> Unit) {
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
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
                    ) { Text(if (game.action == Action.Install) "Install" else "Convert", fontSize = 13.sp) }
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
                // Needs the original game file, so not for games found only as installed; nothing to redo for as-is installs.
                if (state == CardState.Done && game.file != null && game.action == Action.Convert) {
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

    /**
     * Asks before removing. An installed game is uninstalled (its saves go with it), with a box to
     * delete the original file too when it is on the phone; a game that is only a file offers
     * to delete that file.
     */
    @Composable
    private fun RemoveDialog(game: Game, installed: Boolean, onDismiss: () -> Unit, onConfirm: (uninstall: Boolean, deleteFile: Boolean) -> Unit) {
        var deleteFile by remember { mutableStateOf(false) }
        val fileName = game.file?.let { "${game.place}/${it.name}" }
        AlertDialog(
            onDismissRequest = onDismiss,
            containerColor = CardColor,
            title = { Text(if (installed) "Remove ${game.label}?" else "Delete ${game.label}'s file?", color = Color(0xFFE8E8EE)) },
            text = {
                Column {
                    Text(
                        if (installed) "This uninstalls the game and its saved progress." else "$fileName will be deleted from this phone.",
                        color = Muted, fontSize = 14.sp,
                    )
                    if (installed && fileName != null) {
                        Row(
                            Modifier.padding(top = 14.dp).clip(RoundedCornerShape(8.dp)).clickable { deleteFile = !deleteFile }
                                .padding(end = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Checkbox(deleteFile, onCheckedChange = { deleteFile = it },
                                colors = CheckboxDefaults.colors(checkedColor = Accent, uncheckedColor = AccentText))
                            Column {
                                Text("Also delete the original file", color = Color(0xFFE8E8EE), fontSize = 14.sp)
                                Text(fileName, color = Muted, fontSize = 12.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            }
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { onConfirm(installed, !installed || deleteFile) }) {
                    Text(if (installed) "Remove" else "Delete", color = ErrorText)
                }
            },
            dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AccentText) } },
        )
    }
}
