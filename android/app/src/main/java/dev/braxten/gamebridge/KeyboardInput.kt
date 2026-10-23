package dev.braxten.gamebridge

import android.content.Context
import android.text.InputType
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager

/**
 * The phone's keyboard for an iPhone game's text fields: an invisible view
 * that takes the keyboard's letters and hands them to the game as they are
 * typed ("\b" deletes, "\n" is return), the way an iPhone keyboard types
 * into the field being edited (ios/src/fw/typing.rs).
 */
class KeyboardInput(context: Context, private val typed: (String) -> Unit) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(info: EditorInfo): InputConnection {
        // No suggestions: the keyboard sends each letter as it is typed
        // instead of holding a word back to correct it.
        info.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        info.imeOptions = EditorInfo.IME_ACTION_DONE or EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                if (text.isNotEmpty()) typed(text.toString())
                return true
            }

            override fun deleteSurroundingText(before: Int, after: Int): Boolean {
                if (before > 0) typed("\b".repeat(before))
                return true
            }

            override fun performEditorAction(action: Int): Boolean {
                typed("\n")
                return true
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                if (event.action == KeyEvent.ACTION_DOWN) key(event)
                return true
            }
        }
    }

    /** A key press from the on-screen keyboard or a real one. */
    private fun key(event: KeyEvent): Boolean {
        when (event.keyCode) {
            KeyEvent.KEYCODE_DEL -> typed("\b")
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> typed("\n")
            else -> {
                val letter = event.unicodeChar
                if (letter == 0) return false
                typed(String(Character.toChars(letter)))
            }
        }
        return true
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent) = key(event) || super.onKeyDown(keyCode, event)

    /** The game started (true) or stopped (false) editing text. */
    fun show(up: Boolean) {
        val keyboard = context.getSystemService(InputMethodManager::class.java)
        if (up) {
            requestFocus()
            keyboard.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        } else {
            keyboard.hideSoftInputFromWindow(windowToken, 0)
            clearFocus()
        }
    }
}
