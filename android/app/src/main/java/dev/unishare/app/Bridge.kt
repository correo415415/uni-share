package dev.unishare.app

import android.Manifest
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.documentfile.provider.DocumentFile
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * `window.Android` inside the web GUI. Everything the browser sandbox cannot do on a phone:
 *  - the user's download folder: by default the public `Downloads/unishare` (MediaStore on
 *    API 29+, plain file + WRITE_EXTERNAL_STORAGE on ≤ 28) or any other folder chosen through
 *    the Storage Access Framework (a `content://` tree). The Rust engine keeps writing to the
 *    app-private dir and finished jobs are exported/copied from there.
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
        .put("downloadTarget", downloadTargetName())
        .put("downloadDefault", DEFAULT_TARGET)
        .put("appDownloadDir", (activity.application as App).downloadDir.absolutePath)
        .toString()

    private fun appVersion(): String = try {
        activity.packageManager.getPackageInfo(activity.packageName, 0).versionName ?: ""
    } catch (_: Exception) {
        ""
    }

    // -------------------------------------------------------------- SAF folder --

    /**
     * Opens `ACTION_OPEN_DOCUMENT_TREE` positioned on the public Download folder; the result
     * arrives via `androidEvent('folder', name)`. Android 11+ refuses the *root* of Download
     * (and the whole storage): the user has to pick or create a sub-folder, which the page
     * explains. Without a picked tree the default `Downloads/unishare` is used (no picker needed).
     */
    @JavascriptInterface
    fun pickDownloadFolder() = activity.runOnUiThread { activity.pickTree(downloadsInitialUri()) }

    private fun downloadsInitialUri(): Uri? = try {
        DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", "primary:" + Environment.DIRECTORY_DOWNLOADS)
    } catch (_: Exception) {
        null
    }

    /** Back to the default public `Downloads/unishare`. */
    @JavascriptInterface
    fun clearDownloadFolder() {
        prefs.edit().remove(KEY_TREE).apply()
        ensureDefaultFolder()
        emit("folder", JSONObject.NULL)
    }

    /**
     * Creates the default `Downloads/unishare` folder up front so it shows in the Files app
     * before anything is received. ≤ API 28: plain `mkdirs()` (needs WRITE_EXTERNAL_STORAGE,
     * so only when already granted); API 29+: MediaStore cannot create an empty directory, so a
     * tiny hidden `.unishare` marker is inserted with `RELATIVE_PATH = Download/unishare` — the
     * insert creates the directory. Idempotent (a preference remembers it per app install; the
     * marker is re-created if the user deleted the folder).
     */
    fun ensureDefaultFolder() {
        if (treeUri() != null || !hasDefaultTargetAccess()) return
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                val resolver = activity.contentResolver
                val rel = Environment.DIRECTORY_DOWNLOADS + "/" + DEFAULT_SUBDIR + "/"
                // Already there? (query our own marker; other apps' files in the folder are not visible to us)
                val exists = resolver.query(
                    MediaStore.Downloads.EXTERNAL_CONTENT_URI, arrayOf(MediaStore.Downloads._ID),
                    "${MediaStore.Downloads.RELATIVE_PATH}=? AND ${MediaStore.Downloads.DISPLAY_NAME}=?", arrayOf(rel, KEEP_FILE), null
                )?.use { it.count > 0 } ?: false
                if (exists) return
                val values = ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, KEEP_FILE)
                    put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                    put(MediaStore.Downloads.RELATIVE_PATH, rel)
                }
                val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return
                resolver.openOutputStream(uri, "w")?.use { it.write("uni-share download folder\n".toByteArray()) }
            } else {
                @Suppress("DEPRECATION")
                val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), DEFAULT_SUBDIR)
                if (!dir.isDirectory) dir.mkdirs()
            }
            if (!prefs.getBoolean("default_folder_created", false)) {
                prefs.edit().putBoolean("default_folder_created", true).apply()
                Native.logBoth(android.util.Log.INFO, "saf", "carpeta por defecto creada: $DEFAULT_TARGET")
            }
        } catch (e: Exception) {
            Native.logBoth(android.util.Log.WARN, "saf", "no se pudo crear $DEFAULT_TARGET: $e")
        }
    }

    /** Human-readable name of the chosen SAF tree, or null when the default is in use. */
    @JavascriptInterface
    fun downloadFolderName(): String? = downloadTreeName()

    /** Where finished receptions end up, as shown in Settings. */
    @JavascriptInterface
    fun downloadTargetName(): String = downloadTreeName() ?: DEFAULT_TARGET

    /**
     * ≤ API 28 needs WRITE_EXTERNAL_STORAGE to create `Downloads/unishare`; 29+ writes through
     * MediaStore without any permission. Returns true when a copy can proceed right now.
     */
    fun hasDefaultTargetAccess(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q ||
        activity.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED

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
        Native.logBoth(android.util.Log.INFO, "saf", "carpeta de descargas: ${downloadTreeName() ?: uri}")
        emit("folder", downloadTreeName() ?: JSONObject.NULL)
    }

    /**
     * Copies the files a finished job wrote (engine → app-private dir) into the user-visible
     * download location, mirroring the sub-folders relative to the engine download dir:
     *  - the SAF tree if one was picked,
     *  - otherwise the public `Downloads/unishare` (MediaStore.Downloads on API 29+; a plain
     *    directory + media scan on ≤ 28, asking for WRITE_EXTERNAL_STORAGE the first time).
     * Called by the page when a reception/download reaches `completed` (`saved` paths from the
     * snapshot). Idempotent per job.
     */
    @JavascriptInterface
    fun exportJob(jobId: Long, savedJson: String, jobName: String) {
        if (prefs.getBoolean("exported.$jobId", false)) return
        val tree = treeUri()
        if (tree == null && !hasDefaultTargetAccess()) {
            // Remember the request; MainActivity re-runs it once the permission is granted.
            pendingExport = Triple(jobId, savedJson, jobName)
            activity.runOnUiThread { activity.requestStorageForDownloads() }
            return
        }
        val paths = JSONArray(savedJson)
        val base = (activity.application as App).downloadDir
        val root = tree?.let { DocumentFile.fromTreeUri(activity, it) }
        if (tree != null && root == null) return
        activity.io.execute {
            var n = 0
            var bytes = 0L
            for (i in 0 until paths.length()) {
                val f = File(paths.getString(i))
                if (!f.isFile) continue
                try {
                    val rel = f.relativeToOrNull(base)?.path ?: f.name
                    val sub = rel.substringBeforeLast('/', "")
                    val ok = if (root != null) copyToTree(root, sub, f) else copyToPublicDownloads(sub, f)
                    if (ok) {
                        n++
                        bytes += f.length()
                    }
                } catch (e: Exception) {
                    Native.logBoth(android.util.Log.WARN, "export", "${f.name}: $e")
                }
            }
            if (n > 0) {
                Native.logBoth(android.util.Log.INFO, "export", "«$jobName»: $n archivo(s), $bytes bytes → ${downloadTargetName()}")
                prefs.edit().putBoolean("exported.$jobId", true).apply()
                emit("exported", JSONObject().put("id", jobId).put("files", n).put("bytes", bytes).put("name", jobName).put("folder", downloadTargetName()))
            } else if (paths.length() > 0) {
                emit("toast", "No se pudo copiar «$jobName» a ${downloadTargetName()}")
            }
        }
    }

    /** Export postponed until WRITE_EXTERNAL_STORAGE (≤ API 28) is granted. */
    private var pendingExport: Triple<Long, String, String>? = null

    fun retryPendingExport() {
        val (id, saved, name) = pendingExport ?: return
        pendingExport = null
        exportJob(id, saved, name)
    }

    private fun copyToTree(root: DocumentFile, sub: String, f: File): Boolean {
        val dir = ensureDirs(root, sub)
        dir.findFile(f.name)?.delete()
        val target = dir.createFile(guessMime(f.name), f.name) ?: return false
        activity.contentResolver.openOutputStream(target.uri, "w")?.use { out ->
            f.inputStream().use { it.copyTo(out) }
        } ?: return false
        return true
    }

    /** `Downloads/unishare[/sub]/name`, visible in the Files/Downloads apps right away. */
    private fun copyToPublicDownloads(sub: String, f: File): Boolean {
        val relDir = DEFAULT_SUBDIR + (if (sub.isNotBlank()) "/$sub" else "")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val resolver = activity.contentResolver
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, f.name)
                put(MediaStore.Downloads.MIME_TYPE, guessMime(f.name))
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/" + relDir)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return false
            try {
                resolver.openOutputStream(uri, "w")?.use { out -> f.inputStream().use { it.copyTo(out) } } ?: throw IllegalStateException("no stream")
                values.clear()
                values.put(MediaStore.Downloads.IS_PENDING, 0)
                resolver.update(uri, values, null, null)
                return true
            } catch (e: Exception) {
                resolver.delete(uri, null, null)
                throw e
            }
        }
        @Suppress("DEPRECATION")
        val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), relDir)
        if (!dir.isDirectory && !dir.mkdirs()) return false
        val target = File(dir, f.name)
        f.inputStream().use { input -> target.outputStream().use { input.copyTo(it) } }
        MediaScannerConnection.scanFile(activity, arrayOf(target.absolutePath), arrayOf(guessMime(f.name)), null)
        return true
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

    // ------------------------------------------------------------ file picker --

    /** Opens the system document picker; the chosen file (or a folder with several) arrives via `androidEvent('picked', path)`. */
    @JavascriptInterface
    fun pickFiles() = activity.runOnUiThread { activity.pickFiles() }

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
        /** Sub-folder inside the public Downloads folder used when no SAF tree is picked. */
        const val DEFAULT_SUBDIR = "unishare"
        /** Shown in Settings (the Files app shows «Descargas» on Spanish devices, the path is `Download/unishare`). */
        const val DEFAULT_TARGET = "Downloads/unishare"
        /** Placeholder written once so the folder exists (and is visible) before the first transfer. */
        const val KEEP_FILE = ".unishare"
    }
}
