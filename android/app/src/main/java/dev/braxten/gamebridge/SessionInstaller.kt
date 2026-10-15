package dev.braxten.gamebridge

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.IntentSender
import android.content.pm.PackageInstaller
import android.os.Build
import android.os.Environment
import java.io.File
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Installs and removes games through Android's own installer, for phones without root. Android
 * shows its confirmation for each install and removal; the user has turned on "Install unknown
 * apps" for Game Bridge in the setup screen, which also lets it copy game data into Android/obb.
 */
class SessionInstaller(context: Context) : Installer {
    private val context = context.applicationContext
    override val keepsOldTargets = false

    override fun install(apks: List<File>): InstallResult {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        // Updates of games this app installed go through without asking again, but only for games
        // made for Android 15 or newer (Android's rule); older ones still ask.
        params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        val id = installer.createSession(params)
        val answer = installer.openSession(id).use { session ->
            try {
                // All pieces in one session: Android installs them as one app or not at all.
                apks.forEachIndexed { index, apk ->
                    apk.inputStream().use { input ->
                        session.openWrite("piece-$index.apk", 0, apk.length()).use { output ->
                            input.copyTo(output)
                            session.fsync(output)
                        }
                    }
                }
            } catch (failure: Exception) {
                session.abandon()
                return InstallResult.Failed("Couldn't hand the game to Android: ${failure.message}")
            }
            val answer = try {
                waitForAnswer("install-$id") { sender -> session.commit(sender) }
            } catch (failure: Exception) {
                session.abandon()
                return InstallResult.Failed("Android didn't take the game: ${failure.message}")
            }
            // Cancelled here, so a forgotten confirmation screen can't still install it later.
            if (answer.status == NO_ANSWER) session.abandon()
            answer
        }
        return when {
            answer.status == PackageInstaller.STATUS_SUCCESS -> InstallResult.Done
            answer.status == PackageInstaller.STATUS_FAILURE_ABORTED -> InstallResult.Failed("The install was cancelled.")
            answer.status == NO_ANSWER -> InstallResult.Failed("Android's install screen got no answer, so the install was cancelled.")
            answer.status == PackageInstaller.STATUS_FAILURE_CONFLICT -> InstallResult.Failed(OTHER_KEY)
            answer.status == PackageInstaller.STATUS_FAILURE_BLOCKED ->
                InstallResult.Failed("This phone doesn't allow installing apps from Game Bridge (a phone policy or restriction).")
            answer.status == PackageInstaller.STATUS_FAILURE_STORAGE -> InstallResult.Failed("Not enough free space on the phone to install it.")
            else -> neededTarget(answer.message)?.let { InstallResult.NeedsTarget(it) }
                ?: InstallResult.Failed("Android didn't install it: ${answer.message ?: "no reason given"}")
        }
    }

    /**
     * Android's refusal of a game made for an old version names the version it needs
     * ("must target at least SDK version 24"). The message is meant for people, so only a
     * plausible number counts.
     */
    private fun neededTarget(message: String?): Int? {
        val number = Regex("at least SDK version (\\d+)").find(message ?: return null)?.groupValues?.get(1)?.toIntOrNull()
        return number?.takeIf { it in 1..Build.VERSION.SDK_INT }
    }

    override fun copyGameData(folder: File): String? {
        val obb = File(Environment.getExternalStorageDirectory(), "Android/obb")
        return try {
            for (source in folder.walkTopDown().filter { it.isFile }) {
                val target = File(obb, source.relativeTo(folder).path)
                target.parentFile!!.mkdirs()
                source.copyTo(target, overwrite = true)
                if (target.length() != source.length()) return "Installed, but the game data didn't copy completely."
            }
            null
        } catch (failure: Exception) {
            "Installed, but the game data couldn't be copied (is \"Install unknown apps\" still on for Game Bridge?): ${failure.message}"
        }
    }

    override fun uninstall(packageName: String): String? {
        val answer = waitForAnswer("uninstall-$packageName") { sender ->
            context.packageManager.packageInstaller.uninstall(packageName, sender)
        }
        return when (answer.status) {
            PackageInstaller.STATUS_SUCCESS -> null
            PackageInstaller.STATUS_FAILURE_ABORTED, NO_ANSWER -> "Not removed."
            else -> "Android didn't remove it: ${answer.message ?: "no reason given"}"
        }
    }

    override fun delete(file: File): String? =
        if (file.delete() || !file.exists()) null else "Couldn't delete ${file.name}. Is \"All files access\" still on for Game Bridge?"

    private class Answer(val status: Int, val message: String?)

    private companion object {
        /** Not one of Android's statuses: nobody answered the confirmation in time. */
        const val NO_ANSWER = -1000
    }

    /**
     * Starts an install or removal and waits for Android's final answer. When Android first needs
     * the user to confirm, it sends its confirmation screen, which is shown here; the answer that
     * matters comes after. Android doesn't let an app open screens while it is in the background,
     * so the screen is also kept in Confirmation for MainActivity to show when the user comes back.
     */
    private fun waitForAnswer(tag: String, start: (IntentSender) -> Unit): Answer {
        val action = "${context.packageName}.STATUS.$tag"
        val answers = LinkedBlockingQueue<Answer>()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(receiver: Context, intent: Intent) {
                val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                    val confirm = if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    else @Suppress("DEPRECATION") intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    if (confirm != null) {
                        confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                        Confirmation.waiting = confirm
                        runCatching { context.startActivity(confirm) }
                        return
                    }
                }
                Confirmation.waiting = null
                answers.put(Answer(status, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)))
            }
        }
        // Not exported: only Android's installer, through this app's own PendingIntent, can answer.
        if (Build.VERSION.SDK_INT >= 33) {
            context.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
        } else {
            context.registerReceiver(receiver, IntentFilter(action))
        }
        try {
            // Mutable: Android adds the status to it (immutable ones are refused for apps targeting 35+).
            val pending = PendingIntent.getBroadcast(
                context, tag.hashCode(), Intent(action).setPackage(context.packageName),
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            start(pending.intentSender)
            return answers.poll(30, TimeUnit.MINUTES) ?: Answer(NO_ANSWER, null)
        } finally {
            Confirmation.waiting = null
            context.unregisterReceiver(receiver)
        }
    }
}

/** Android's confirmation screen for the install or removal waiting on the user, if any. */
object Confirmation {
    @Volatile
    var waiting: Intent? = null
}
