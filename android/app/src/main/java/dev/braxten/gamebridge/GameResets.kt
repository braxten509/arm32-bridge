package dev.braxten.gamebridge

import java.io.File

/**
 * Resetting an iPhone game's progress (its card's "Reset progress…"): its home folder
 * (<root>/ios/<name>, where its saves are) moves to <root>/ios-reset/<name> as the one undo copy,
 * so the game starts fresh; "Undo reset" puts that copy back. The game must be closed first.
 * Plain files only (no Context), so tools/check-resets.sh can check it on the PC.
 */
object GameResets {
    private fun home(root: File, name: String) = File(File(root, "ios"), name)
    private fun undoCopy(root: File, name: String) = File(File(root, "ios-reset"), name)
    private fun safe(name: String) = name.isNotEmpty() && '/' !in name && name != "." && name != ".."

    /** A reset of this game can be undone. */
    fun canUndo(root: File, name: String): Boolean = safe(name) && undoCopy(root, name).isDirectory

    /** Moves the game's saves aside, replacing an older undo copy; null when done, else why not. */
    fun reset(root: File, name: String): String? {
        if (!safe(name)) return "That game's folder name can't be used."
        val home = home(root, name)
        if (!home.isDirectory || home.list().isNullOrEmpty()) return "There is no progress to reset."
        val copy = undoCopy(root, name)
        if (copy.exists() && !copy.deleteRecursively()) return "The older undo copy couldn't be removed."
        copy.parentFile!!.mkdirs()
        if (!home.renameTo(copy)) return "The saves couldn't be moved aside."
        return null
    }

    /** Puts the saves from before the last reset back (the ones made since go); null when done, else why not. */
    fun undo(root: File, name: String): String? {
        if (!canUndo(root, name)) return "There is no reset to undo."
        val home = home(root, name)
        val copy = undoCopy(root, name)
        // The saves made since the reset go aside first, so a failed move leaves them in place.
        val since = File(home.parentFile, "$name.since-reset")
        since.deleteRecursively()
        if (home.exists() && !home.renameTo(since)) return "The current saves couldn't be moved."
        if (!copy.renameTo(home)) {
            since.renameTo(home)
            return "The saves couldn't be put back."
        }
        since.deleteRecursively()
        return null
    }
}
