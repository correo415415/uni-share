package dev.unishare.app

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.documentfile.provider.DocumentFile
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * `window.Android` inside the web GUI. Everything the browser sandbox cannot do on a phone:
 *  - the user's download folder through the Storage Access Framework (a `content://` tree;
 *    the Rust engine keeps writing to the app-private dir and finished jobs are exported),
 *  - the QR scanner (zxing) for `.unishare` tickets / pairing tickets,
 *  - the system share sheet for links and tickets.
 *
 * The JS side only calls these when `window.Android` exists (see src/gui/app.js → `mobile`).
 * Results that need the UI thread come back through `openNew(...)` / `androidEvent(...)`.
 */
class Bridge(private val activity: MainActivity, private val web: WebView) {

    private val prefs get() = activity.getSharedPreferences("uni-share", Context.MODE_PRIVATE)

    // ------------------------------------------------------------- capabilities --

    /** Lets the page tune its layout/buttons. */
    @JavascriptInterface
    fun info(): String = JSONObject()
        .put("platform", "android")
        .put("version", appVersion())
        .put("sdk", android.os.Build.VERSION.SDK_INT)
        .put("downloadTree", downloadTreeName() ?: JSONObject.NULL)
        .put("appDownloadDir", (activity.application as App).downloadDir.absolutePath)
        .toString()

    private fun appVersion(): String = try {
        activity.packageManager.getPackageInfo(activity.packageName, 0).versionName ?: ""
    } catch (_: Exception) {
        ""
    }

    // -------------------------------------------------------------- SAF folder --

    /** Opens `ACTION_OPEN_DOCUMENT_TREE`; the result arrives via `androidEvent('folder', name)`. */
    @JavascriptInterface
    fun pickDownloadFolder() = activity.runOnUiThread { activity.pickTree() }

    @JavascriptInterface
    fun clearDownloadFolder() {
        prefs.edit().remove(KEY_TREE).apply()
        emit("folder", JSONObject.NULL)
    }

    /** Human-readable name of the chosen tree (e.g. `Descargas/uni-share`), or null. */
    @JavascriptInterface
    fun downloadFolderName(): String? = downloadTreeName()

    private fun treeUri(): Uri? = prefs.getString(KEY_TREE, null)?.let(Uri::parse)?.takeIf { uri ->
        // The grant survives reboots only if we hold a persisted permission.
        activity.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isWritePermission }
    }

    private fun downloadTreeName(): String? = treeUri()?.let { uri ->
        val doc = DocumentFile.fromTreeUri(activity, uri)
        doc?.name ?: uri.lastPathSegment?.substringAfterLast(':')
    }

    fun onTreePicked(uri: Uri) {
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        try {
            activity.contentResolver.takePersistableUriPermission(uri, flags)
        } catch (_: Exception) {
        }
        prefs.edit().putString(KEY_TREE, uri.toString()).apply()
        emit("folder", downloadTreeName() ?: JSONObject.NULL)
    }

    /**
     * Copies the files a finished job wrote (engine → app-private dir) into the SAF tree,
     * mirroring the sub-folders relative to the engine download dir. Called by the page when a
     * reception/download reaches `completed` (`saved` paths from the snapshot). Idempotent per job.
     */
    @JavascriptInterface
    fun exportJob(jobId: Long, savedJson: String, jobName: String) {
        val tree = treeUri() ?: return
        if (prefs.getBoolean("exported.$jobId", false)) return
        val paths = JSONArray(savedJson)
        val root = DocumentFile.fromTreeUri(activity, tree) ?: return
        val base = (activity.application as App).downloadDir
        activity.io.execute {
            var n = 0
            var bytes = 0L
            for (i in 0 until paths.length()) {
                val f = File(paths.getString(i))
                if (!f.isFile) continue
                try {
                    val rel = f.relativeToOrNull(base)?.path ?: f.name
                    val dir = ensureDirs(root, rel.substringBeforeLast('/', ""))
                    val mime = guessMime(f.name)
                    dir.findFile(f.name)?.delete()
                    val target = dir.createFile(mime, f.name) ?: continue
                    activity.contentResolver.openOutputStream(target.uri, "w")?.use { out ->
                        f.inputStream().use { it.copyTo(out) }
                    }
                    n++
                    bytes += f.length()
                } catch (_: Exception) {
                }
            }
            if (n > 0) {
                prefs.edit().putBoolean("exported.$jobId", true).apply()
                emit("exported", JSONObject().put("id", jobId).put("files", n).put("bytes", bytes).put("name", jobName).put("folder", downloadTreeName() ?: ""))
            }
        }
    }

    private fun ensureDirs(root: DocumentFile, rel: String): DocumentFile {
        var cur = root
        for (seg in rel.split('/').filter { it.isNotBlank() }) {
            cur = cur.findFile(seg)?.takeIf { it.isDirectory } ?: (cur.createDirectory(seg) ?: return cur)
        }
        return cur
    }

    private fun guessMime(name: String): String =
        android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
            ?: "application/octet-stream"

    // -------------------------------------------------------------------- QR --

    /** Launches the camera scanner; the decoded text is routed by MainActivity.onQr → `openNew`. */
    @JavascriptInterface
    fun scanQr() = activity.runOnUiThread { activity.scanQr() }

    // ----------------------------------------------------------------- share --

    @JavascriptInterface
    fun shareText(text: String, title: String) = activity.runOnUiThread {
        val i = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_TEXT, text)
            putExtra(Intent.EXTRA_SUBJECT, title)
        }
        try {
            activity.startActivity(Intent.createChooser(i, title))
        } catch (_: Exception) {
        }
    }

    /** Shares a file the engine wrote (ticket `.unishare`, received file) through FileProvider. */
    @JavascriptInterface
    fun shareFile(path: String, title: String) = activity.runOnUiThread {
        val f = File(path)
        if (!f.isFile) return@runOnUiThread
        try {
            val uri = androidx.core.content.FileProvider.getUriForFile(activity, activity.packageName + ".files", f)
            val i = Intent(Intent.ACTION_SEND).apply {
                type = guessMime(f.name)
                putExtra(Intent.EXTRA_STREAM, uri)
                putExtra(Intent.EXTRA_SUBJECT, title)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            activity.startActivity(Intent.createChooser(i, title))
        } catch (_: Exception) {
        }
    }

    // -------------------------------------------------------------- internals --

    /** `androidEvent(kind, payload)` on the page (defined in app.js). */
    fun emit(kind: String, payload: Any?) = activity.runOnUiThread {
        val p = when (payload) {
            null, JSONObject.NULL -> "null"
            is JSONObject, is JSONArray -> payload.toString()
            else -> JSONObject.quote(payload.toString())
        }
        web.evaluateJavascript("window.androidEvent && androidEvent(${JSONObject.quote(kind)}, $p)", null)
    }

    companion object {
        const val KEY_TREE = "download_tree"
    }
}
