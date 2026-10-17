package dev.braxten.gamebridge

import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.text.format.DateUtils
import android.view.WindowManager
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.layout.layout
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.offset
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import kotlin.math.floor
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
private val NoticeText = Color(0xFFFFCC80)

/** The space around a checkbox's box inside its 48 dp touch area: a checkbox row moves left by it, so the box lines up with the text above. */
private val CheckboxInset = 14.dp

/** The space between a text button's edge and its text. */
private val TextButtonInset = 12.dp

/** One turn of the refresh arrow while a search runs. */
private const val SPIN_MILLIS = 900

/** What a game's card shows. */
sealed class CardState {
    object Ready : CardState()
    class Working(val progress: Progress) : CardState()
    object Done : CardState()
    /** `replace`: it was a replace, so trying again replaces again. */
    class Failed(val reason: String, val replace: Boolean = false) : CardState()

    /** Another copy of the game, signed by someone else, is installed: Android won't install over it. */
    object OtherCopy : CardState()
}

/** What the setup screen shows: which of Android's two permissions the user has given. */
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

    override fun onStart() {
        super.onStart()
        Confirmation.appVisible = true
    }

    override fun onStop() {
        Confirmation.appVisible = false
        super.onStop()
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

    /** Each card's button for the games in `found`, from what is installed now (a card that is converting or failed keeps its state). */
    private fun updateStates(found: List<Game>) {
        val states = Conversions.states
        for (game in found) {
            val state = states[game.packageName]
            if (state !is CardState.Working && state !is CardState.Failed) {
                states[game.packageName] = when {
                    game.action == Action.Install && Games.isInstalled(this, game.packageName) -> CardState.Done
                    game.action == Action.Install -> CardState.Ready
                    Games.isConverted(this, game.packageName) -> CardState.Done
                    // Installed but not by Game Bridge: the original from elsewhere.
                    Games.isInstalled(this, game.packageName) -> CardState.OtherCopy
                    else -> CardState.Ready
                }
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
        // Something changed during a search (a removal): search once more when it ends.
        var searchAgain by remember { mutableStateOf(false) }
        val states = Conversions.states
        val busy = Conversions.busy

        fun refresh() {
            if (searching) {
                searchAgain = true
                return
            }
            searching = true
            searchAgain = false
            scope.launch {
                val result = withContext(Dispatchers.IO) {
                    val needed = Setup(Environment.isExternalStorageManager(), packageManager.canRequestPackageInstalls())
                    if (!needed.files || !needed.installs) return@withContext null to needed
                    Games.scan(this@MainActivity).also(::updateStates) to null
                }
                result.first?.let { games = it }
                setup = result.second
                searching = false
                if (searchAgain) refresh()
                // Android/obb opens up only in a process started with the switch on.
                if (setup == null && installsAtStart == false) restart()
            }
        }

        // The list the last search found shows at once; the search below then adds and removes cards.
        LaunchedEffect(Unit) {
            val saved = withContext(Dispatchers.IO) { Games.saved(this@MainActivity)?.also(::updateStates) }
            if (games == null && saved != null) games = saved
            refresh()
        }

        fun convert(game: Game) =
            Conversions.convert(this@MainActivity, game, replace = (states[game.packageName] as? CardState.Failed)?.replace == true)

        // The game whose Replace dialog is showing.
        var replacing by remember { mutableStateOf<Game?>(null) }
        replacing?.let { game ->
            ReplaceDialog(game, onDismiss = { replacing = null }) {
                replacing = null
                Conversions.convert(this@MainActivity, game, replace = true)
            }
        }

        // The game whose Remove dialog is showing.
        var removing by remember { mutableStateOf<Game?>(null) }

        /** Uninstalls the game and/or deletes its file, then searches again so the card updates or goes. */
        fun remove(game: Game, uninstall: Boolean, deleteFile: Boolean) {
            scope.launch {
                val problems = withContext(Dispatchers.IO) {
                    listOfNotNull(
                        if (uninstall) Installer(this@MainActivity).uninstall(game.packageName) else null,
                        if (deleteFile) game.file?.let(Installer(this@MainActivity)::delete) else null,
                    )
                }
                problems.firstOrNull()?.let { Toast.makeText(this@MainActivity, it, Toast.LENGTH_LONG).show() }
                states.remove(game.packageName)
                // Gone from the phone: the card goes now, not when the search below ends.
                val fileGone = game.file?.exists() != true
                if (fileGone && !Games.isInstalled(this@MainActivity, game.packageName)) {
                    games = games?.filterNot { it.packageName == game.packageName && it.file == game.file }
                }
                refresh()
            }
        }

        removing?.let { game ->
            RemoveDialog(game, installed = Games.isInstalled(this, game.packageName), onDismiss = { removing = null }) { uninstall, deleteFile ->
                removing = null
                remove(game, uninstall, deleteFile)
            }
        }

        // The backup window, and the restore window opened from it.
        var backingUp by remember { mutableStateOf(false) }
        var restoring by remember { mutableStateOf(false) }
        // A long backup stops if the phone sleeps and closes the app, so the screen stays on meanwhile.
        LaunchedEffect(Backups.status != null) {
            if (Backups.status != null) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        if (backingUp) BackupDialog(games.orEmpty(), onDismiss = { backingUp = false }, onRestore = { backingUp = false; restoring = true })
        if (restoring) RestoreDialog(onDismiss = { restoring = false }, onRestored = ::refresh)

        // Searching the whole phone takes a moment, so only when the app opens (above) and on the refresh button.
        // Back from Android's settings: check the permissions again.
        LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
            if (setup != null) refresh()
            // An install or removal that finished converting while the app was in the background:
            // Android wouldn't show its confirmation then, so it shows now (once).
            Confirmation.waiting.getAndSet(null)?.let { runCatching { startActivity(it) } }
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
                BackupButton(enabled = setup == null) { backingUp = true }
                Spacer(Modifier.width(8.dp))
                RefreshButton(enabled = !searching && setup == null, spinning = searching && setup == null, onClick = ::refresh)
            }
            Text(
                "Game APKs, XAPKs and data files on this phone.",
                color = Muted,
                fontSize = 13.sp,
                modifier = Modifier.padding(start = 18.dp, bottom = 14.dp),
            )
            when {
                setup != null -> SetupSteps(setup!!)
                games == null -> Message("Looking for games…")
                games!!.isEmpty() -> Message("No games or game bundles on this phone. Download a game's APK, XAPK, APKM or APKS file, then tap ↻.")
                else -> LazyVerticalGrid(
                    columns = GridCells.Fixed(2),
                    contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 24.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(games!!, key = { it.file?.path ?: it.packageName }) { game ->
                        GameCard(game, states[game.packageName] ?: CardState.Ready, busy, onConvert = { convert(game) }, onReplace = { replacing = game }, onRemove = { removing = game }, onOpen = {
                            packageManager.getLaunchIntentForPackage(game.packageName)?.let(::startActivity)
                        })
                    }
                }
            }
        }
    }

    /**
     * The two permissions Game Bridge needs, each with a button to its page in Android's
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
    private fun RefreshButton(enabled: Boolean, spinning: Boolean, onClick: () -> Unit) {
        // Turns while a search runs, counterclockwise like the arrow points; when it ends, the turn
        // finishes instead of jumping back.
        val turn = remember { Animatable(0f) }
        LaunchedEffect(spinning) {
            if (spinning) {
                while (true) turn.animateTo(turn.value - 360f, tween(SPIN_MILLIS, easing = LinearEasing))
            } else {
                val end = floor(turn.value / 360f) * 360f
                turn.animateTo(end, tween(((turn.value - end) / 360f * SPIN_MILLIS).toInt(), easing = LinearEasing))
                turn.snapTo(0f)
            }
        }
        Box(
            Modifier.size(40.dp).clip(CircleShape).background(CardColor).clickable(enabled = enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            Box(Modifier.rotate(turn.value)) { RefreshIcon(if (enabled || spinning) AccentText else Muted) }
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
    private fun GameCard(game: Game, state: CardState, busy: Boolean, onConvert: () -> Unit, onReplace: () -> Unit, onRemove: () -> Unit, onOpen: () -> Unit) {
        // Press and hold a card for its menu; a running conversion can't be removed under itself.
        var menu by remember { mutableStateOf(false) }
        // A failed card shows a short line; tapping the card shows the whole reason.
        var why by remember { mutableStateOf(false) }
        // Likewise for a game whose data file isn't in place.
        var dataWhy by remember { mutableStateOf(false) }
        if (dataWhy) DataDialog(game, onDismiss = { dataWhy = false })
        if (why && state is CardState.Failed) {
            AlertDialog(
                onDismissRequest = { why = false },
                containerColor = CardColor,
                title = { Text("${game.label} didn't install", color = Color(0xFFE8E8EE)) },
                text = { Text(state.reason, color = Muted, fontSize = 14.sp) },
                confirmButton = { TextButton(onClick = { why = false }) { Text("OK", color = AccentText) } },
            )
        }
        Box {
            Column(
                Modifier.clip(RoundedCornerShape(16.dp)).background(CardColor)
                    .combinedClickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                        onClick = {
                            when {
                                state is CardState.Failed -> why = true
                                state == CardState.OtherCopy && !busy -> onReplace()
                                needsDataNotice(game, state) -> dataWhy = true
                            }
                        },
                        onLongClick = { if (state !is CardState.Working) menu = true },
                    )
                    .padding(14.dp).fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) { CardContent(game, state, busy, onConvert, onReplace, onOpen) }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }, containerColor = Track) {
                val close = { menu = false }
                if (state == CardState.Done) MenuItem("Open") { close(); onOpen() }
                if (game.file != null && !busy) when (state) {
                    CardState.Ready -> MenuItem(if (game.action == Action.Install) "Install" else "Convert") { close(); onConvert() }
                    CardState.Done -> if (game.action == Action.Convert) MenuItem("Convert again") { close(); onConvert() }
                    is CardState.Failed -> MenuItem("Try again") { close(); onConvert() }
                    CardState.OtherCopy -> MenuItem("Replace") { close(); onReplace() }
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
    private fun CardContent(game: Game, state: CardState, busy: Boolean, onConvert: () -> Unit, onReplace: () -> Unit, onOpen: () -> Unit) {
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
            // Same line in every state, so a failure doesn't make the card taller than its neighbour.
            if (state is CardState.Failed) {
                Text("Didn't install · tap for why", color = ErrorText, fontSize = 11.sp, maxLines = 1,
                    overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 3.dp))
            } else if (state == CardState.OtherCopy) {
                Text("Already installed · tap to replace", color = NoticeText, fontSize = 11.sp, maxLines = 1,
                    overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 3.dp))
            } else if (needsDataNotice(game, state)) {
                Text("Needs game data · tap", color = NoticeText, fontSize = 11.sp, maxLines = 1,
                    overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 3.dp))
            } else {
                // Where the game file is until it is installed.
                Text(if (state == CardState.Done) "Installed" else game.place, color = Muted, fontSize = 11.sp, maxLines = 1,
                    modifier = Modifier.padding(top = 3.dp))
            }
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
                    CardState.OtherCopy -> Button(
                        onClick = onReplace,
                        enabled = !busy,
                        colors = ButtonDefaults.buttonColors(containerColor = Accent),
                        contentPadding = PaddingValues(0.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) { Text("Replace", fontSize = 13.sp) }
                    is CardState.Failed -> OutlinedButton(
                        onClick = onConvert,
                        enabled = !busy,
                        contentPadding = PaddingValues(0.dp),
                        modifier = Modifier.fillMaxSize(),
                    ) { Text("Try again", color = AccentText, fontSize = 13.sp) }
                }
            }
        }
    }

    /**
     * A game that needs a separate data file shows a notice when the file isn't in place: before
     * converting only when it isn't on the phone at all (else it is copied on install), after
     * installing whenever it is still missing.
     */
    private fun needsDataNotice(game: Game, state: CardState): Boolean =
        state !is CardState.Working && !game.dataInPlace && (game.looseData == null || state == CardState.Done)

    /** What the game data file is and what to do about it. */
    @Composable
    private fun DataDialog(game: Game, onDismiss: () -> Unit) {
        val known = Games.knownData[game.dataFile]
        val size = known?.let { " For this game it's about ${it.size / 1_000_000} MB." } ?: " It is usually a few hundred MB."
        val next = when (val found = game.looseData) {
            null -> "Get it from the same place you got the game. Put the file, or the .zip it came in, in Downloads. " +
                "Then tap ↻ and Convert: Game Bridge finds it, checks it and puts it where the game looks for it."
            else -> "Found it in ${found.where}. Press and hold the card, tap Convert again, and Game Bridge puts it where the game looks for it."
        }
        AlertDialog(
            onDismissRequest = onDismiss,
            containerColor = CardColor,
            title = { Text("${game.label} needs its game data", color = Color(0xFFE8E8EE)) },
            text = {
                Column {
                    Text("Besides the APK, this game comes with a second, big file holding its pictures, sounds and levels. " +
                        "Download sites call it the \"cache\", \"OBB\" or \"data\" file; it ends in .obb and often comes " +
                        "in a .zip together with the APK.$size Without it the game stops while loading.",
                        color = Muted, fontSize = 14.sp)
                    Text(next, color = Color(0xFFE8E8EE), fontSize = 14.sp, modifier = Modifier.padding(vertical = 10.dp))
                    Text("Its exact name: ${game.dataFile.orEmpty()}", color = Muted, fontSize = 12.sp)
                }
            },
            confirmButton = { TextButton(onClick = onDismiss) { Text("OK", color = AccentText) } },
        )
    }

    /**
     * Asks before replacing a copy of the game that came from elsewhere: Android won't install a
     * copy signed by Game Bridge over it, so it has to go, and its saved progress with it.
     */
    @Composable
    private fun ReplaceDialog(game: Game, onDismiss: () -> Unit, onConfirm: () -> Unit) {
        AlertDialog(
            onDismissRequest = onDismiss,
            containerColor = CardColor,
            title = { Text("Replace ${game.label}?", color = Color(0xFFE8E8EE)) },
            text = {
                Text(
                    "${game.label} is already installed from somewhere else, and Android only lets one copy in. " +
                        "Game Bridge first makes its copy, then removes the installed one (Android asks you) and installs " +
                        "its copy. Saved progress is deleted with the old copy, unless the game saves it online. " +
                        "Updates from the Play Store stop for this game.",
                    color = Muted, fontSize = 14.sp,
                )
            },
            confirmButton = { TextButton(onClick = onConfirm) { Text("Replace", color = ErrorText) } },
            dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AccentText) } },
        )
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
                            Modifier.padding(top = 14.dp).bleed(start = CheckboxInset).clip(RoundedCornerShape(8.dp)).clickable { deleteFile = !deleteFile }
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
                Row(Modifier.bleed(end = TextButtonInset), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = onDismiss) { Text("Cancel", color = AccentText) }
                    TextButton(onClick = { onConfirm(installed, !installed || deleteFile) }) {
                        Text(if (installed) "Remove" else "Delete", color = ErrorText)
                    }
                }
            },
        )
    }

    /** The round backup button next to refresh: an arrow going up out of a tray. */
    @Composable
    private fun BackupButton(enabled: Boolean, onClick: () -> Unit) {
        Box(
            Modifier.size(40.dp).clip(CircleShape).background(CardColor).clickable(enabled = enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            val color = if (enabled) AccentText else Muted
            Canvas(Modifier.size(24.dp)) {
                val stroke = 2.6.dp.toPx()
                val style = Stroke(stroke, cap = StrokeCap.Round, join = androidx.compose.ui.graphics.StrokeJoin.Round)
                val left = size.width * 0.2f
                val right = size.width * 0.8f
                val bottom = size.height * 0.82f
                // The tray.
                drawPath(Path().apply {
                    moveTo(left, size.height * 0.6f)
                    lineTo(left, bottom)
                    lineTo(right, bottom)
                    lineTo(right, size.height * 0.6f)
                }, color, style = style)
                // The arrow.
                val top = size.height * 0.16f
                drawLine(color, Offset(center.x, size.height * 0.64f), Offset(center.x, top), stroke, StrokeCap.Round)
                drawPath(Path().apply {
                    moveTo(center.x - size.width * 0.2f, top + size.height * 0.2f)
                    lineTo(center.x, top)
                    lineTo(center.x + size.width * 0.2f, top + size.height * 0.2f)
                }, color, style = style)
            }
        }
    }

    /**
     * Backs up every converted game with its game data files to the folder picked with Android's
     * folder picker; the box adds the original downloaded files. Restore opens the restore window.
     */
    @Composable
    private fun BackupDialog(games: List<Game>, onDismiss: () -> Unit, onRestore: () -> Unit) {
        val context = this@MainActivity
        var folder by remember { mutableStateOf(Backups.folder(context)) }
        var originals by remember { mutableStateOf(Backups.run { loadChoices(context); keepOriginals }) }
        var converted by remember { mutableStateOf<List<Pair<String, java.io.File?>>?>(null) }
        LaunchedEffect(Unit) { converted = withContext(Dispatchers.IO) { Backups.convertedGames(context, games) } }
        var folderName by remember { mutableStateOf(Backups.savedFolderName(context)) }
        LaunchedEffect(folder) { folder?.let { folderName = withContext(Dispatchers.IO) { Backups.folderName(context, it) } } }
        val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree ->
            if (tree != null) {
                Backups.setFolder(context, tree)
                folderName = null
                folder = Backups.folder(context)
            }
        }
        val status = Backups.status
        // Whether a backup ran while this window was open, so the button that closes it says Done afterwards.
        var ran by remember { mutableStateOf(status != null) }
        LaunchedEffect(status) { if (status != null) ran = true }
        val count = converted?.size
        AlertDialog(
            onDismissRequest = onDismiss,
            containerColor = CardColor,
            title = { Text("Back up games", color = Color(0xFFE8E8EE)) },
            text = {
                Column {
                    if (status != null) {
                        Text(status, color = Color(0xFFE8E8EE), fontSize = 14.sp)
                        Text("Keep Game Bridge open until it finishes.", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp))
                        return@Column
                    }
                    val what = when (count) {
                        null -> "Looking for converted games…"
                        0 -> "No converted games are installed yet."
                        1 -> "1 game and its data files go to:"
                        else -> "$count games and their data files go to:"
                    }
                    Text(what, color = Muted, fontSize = 14.sp)
                    Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(if (folder == null) "No folder chosen" else folderName ?: "Loading…", color = Color(0xFFE8E8EE), fontSize = 14.sp, maxLines = 2,
                            overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                        TextButton(onClick = { pick.launch(null) }, modifier = Modifier.bleed(end = TextButtonInset)) { Text(if (folder == null) "Choose" else "Change", color = AccentText) }
                    }
                    Row(
                        Modifier.padding(top = 4.dp).bleed(start = CheckboxInset).clip(RoundedCornerShape(8.dp)).clickable {
                            originals = !originals
                            Backups.setKeepOriginals(context, originals)
                        }.padding(end = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Checkbox(originals, onCheckedChange = { originals = it; Backups.setKeepOriginals(context, it) },
                            colors = CheckboxDefaults.colors(checkedColor = Accent, uncheckedColor = AccentText))
                        Text("Also back up the original files", color = Color(0xFFE8E8EE), fontSize = 14.sp)
                    }
                    val last = Backups.lastBackup(context)
                    if (last > 0) {
                        Text("Last backup: ${DateUtils.getRelativeTimeSpanString(last)}", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp))
                    }
                    Backups.problem?.let { Text(it, color = ErrorText, fontSize = 13.sp, modifier = Modifier.padding(top = 8.dp)) }
                }
            },
            // All three spread over the full width: Restore's text starts where the text above
            // starts and Back up's ends where it ends (bleed undoes the buttons' inner padding).
            confirmButton = {
                val canBackUp = status == null && folder != null && (count ?: 0) > 0
                val canRestore = status == null && folder != null
                val close = when {
                    status != null -> "Hide"
                    ran && Backups.problem == null -> "Done"
                    else -> "Cancel"
                }
                Row(Modifier.bleed(TextButtonInset, TextButtonInset).fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    TextButton(enabled = canRestore, onClick = onRestore) { Text("Restore", color = if (canRestore) AccentText else Muted) }
                    TextButton(onClick = onDismiss) { Text(close, color = AccentText) }
                    TextButton(enabled = canBackUp, onClick = { Backups.backUp(context, converted.orEmpty(), originals) }) {
                        Text("Back up", color = if (canBackUp) AccentText else Muted)
                    }
                }
            },
        )
    }

    /**
     * Lets a control reach out past its slot by its own see-through padding, so the visible part
     * (a button's text, a checkbox's box) lines up with the dialog's text edges.
     */
    private fun Modifier.bleed(start: Dp = 0.dp, end: Dp = 0.dp) = layout { measurable, constraints ->
        val (left, extra) = start.roundToPx() to (start + end).roundToPx()
        val placeable = measurable.measure(constraints.offset(horizontal = extra))
        layout(placeable.width - extra, placeable.height) { placeable.place(-left, 0) }
    }

    /** The games in the backup folder, each with Restore (or "Installed" when that version is). */
    @Composable
    private fun RestoreDialog(onDismiss: () -> Unit, onRestored: () -> Unit) {
        val context = this@MainActivity
        val scope = rememberCoroutineScope()
        var saved by remember { mutableStateOf<List<SavedGame>?>(null) }
        // The game being restored and how far it is, or its problem afterwards.
        var working by remember { mutableStateOf<String?>(null) }
        var progress by remember { mutableStateOf("") }
        var problem by remember { mutableStateOf<String?>(null) }
        LaunchedEffect(Unit) { saved = withContext(Dispatchers.IO) { runCatching { Backups.list(context) }.getOrDefault(emptyList()) } }
        AlertDialog(
            onDismissRequest = { if (working == null) onDismiss() },
            containerColor = CardColor,
            title = { Text("Restore games", color = Color(0xFFE8E8EE)) },
            text = {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    val list = saved
                    when {
                        list == null -> Text("Reading the backup…", color = Muted, fontSize = 14.sp)
                        list.isEmpty() -> Text("No finished backups in this folder.", color = Muted, fontSize = 14.sp)
                    }
                    problem?.let { Text(it, color = ErrorText, fontSize = 13.sp, modifier = Modifier.padding(bottom = 8.dp)) }
                    list.orEmpty().forEach { game ->
                        val current = remember(game, working) { game.inPlace(context) }
                        Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(36.dp).clip(RoundedCornerShape(8.dp)).background(Track)) {
                                game.icon?.let { Image(it.asImageBitmap(), contentDescription = null, modifier = Modifier.fillMaxSize()) }
                            }
                            Column(Modifier.weight(1f).padding(horizontal = 10.dp)) {
                                Text(game.label, color = Color(0xFFE8E8EE), fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                val detail = if (working == game.packageName) progress
                                    else listOf(game.versionName.takeIf { it.isNotEmpty() }?.let { "v$it" }, android.text.format.Formatter.formatShortFileSize(context, game.size))
                                        .filterNotNull().joinToString(" · ")
                                Text(detail, color = Muted, fontSize = 11.sp, maxLines = 1)
                            }
                            TextButton(
                                enabled = working == null && !current,
                                onClick = {
                                    working = game.packageName
                                    problem = null
                                    progress = "Starting…"
                                    scope.launch {
                                        problem = withContext(Dispatchers.IO) {
                                            Backups.restore(context, game) { text -> scope.launch { progress = text } }
                                        }
                                        working = null
                                        onRestored()
                                    }
                                },
                            ) { Text(if (current) "Installed" else "Restore", color = if (working == null && !current) AccentText else Muted) }
                        }
                    }
                }
            },
            confirmButton = { TextButton(enabled = working == null, onClick = onDismiss) { Text("Close", color = AccentText) } },
        )
    }
}
