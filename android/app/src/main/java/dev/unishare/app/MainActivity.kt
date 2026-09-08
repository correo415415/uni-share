package dev.unishare.app

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import java.io.File
import java.util.concurrent.Executors

/**
 * Thin shell: starts the engine (foreground service) and shows the web GUI served by the
 * Rust core on 127.0.0.1. Share-sheet files / unishare: links / .unishare tickets arriving
 * through intents are forwarded to the page via `openNew(...)` (see src/gui/app.js).
 */
class MainActivity : AppCompatActivity() {

    private lateinit var root: FrameLayout
    private lateinit var web: WebView
    private lateinit var splash: View
    private lateinit var error: View
    private var loaded = false
    private var pendingJs: String? = null
    private val io = Executors.newSingleThreadExecutor()

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, true)
        val bg = ContextCompat.getColor(this, R.color.bg)

        root = FrameLayout(this).apply { setBackgroundColor(bg) }
        web = WebView(this).apply {
            setBackgroundColor(bg)
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.allowFileAccess = false
            settings.mediaPlaybackRequiresUserGesture = false
            settings.setSupportZoom(false)
            webChromeClient = WebChromeClient()
            webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    val u = request.url
                    // Keep the app inside the WebView; open everything else in the system browser.
                    if (u.host == "127.0.0.1") return false
                    return try {
                        startActivity(Intent(Intent.ACTION_VIEW, u)); true
                    } catch (_: Exception) {
                        false
                    }
                }

                override fun onPageFinished(view: WebView, url: String) {
                    loaded = true
                    splash.visibility = View.GONE
                    web.visibility = View.VISIBLE
                    pendingJs?.let { web.evaluateJavascript(it, null); pendingJs = null }
                }
            }
            visibility = View.INVISIBLE
        }
        splash = buildSplash()
        error = buildError().apply { visibility = View.GONE }
        root.addView(web, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        root.addView(splash)
        root.addView(error)
        setContentView(root)

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (web.canGoBack()) web.goBack() else moveTaskToBack(true)
            }
        })

        requestNotificationPermission()
        boot()
        handleIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    override fun onDestroy() {
        io.shutdown()
        super.onDestroy()
    }

    // ------------------------------------------------------------------ engine --

    private fun boot() {
        splash.visibility = View.VISIBLE
        error.visibility = View.GONE
        EngineService.start(this)
        io.execute {
            val port = EngineService.ensureStarted(applicationContext)
            runOnUiThread {
                if (port > 0) {
                    web.loadUrl("http://127.0.0.1:$port/")
                } else {
                    splash.visibility = View.GONE
                    error.visibility = View.VISIBLE
                }
            }
        }
    }

    // ----------------------------------------------------------------- intents --

    private fun handleIntent(i: Intent?) {
        i ?: return
        val js: String? = when (i.action) {
            Intent.ACTION_SEND -> {
                val uri = i.getParcelableUri(Intent.EXTRA_STREAM)
                val text = i.getStringExtra(Intent.EXTRA_TEXT)
                when {
                    uri != null -> importToCache(uri)?.let { "openNew('lan', {path: ${jsStr(it)}})" }
                    !text.isNullOrBlank() && looksLikeLink(text) -> "openNew('download', {url: ${jsStr(text.trim())}})"
                    else -> null
                }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val uris = i.getParcelableUris(Intent.EXTRA_STREAM)
                val dir = importManyToCache(uris)
                dir?.let { "openNew('lan', {path: ${jsStr(it)}})" }
            }
            Intent.ACTION_VIEW -> {
                val u = i.data
                when {
                    u == null -> null
                    u.scheme == "unishare" -> "openNew('download', {url: ${jsStr(u.toString())}})"
                    else -> importToCache(u)?.let { "openNew('download', {url: ${jsStr(it)}})" }
                }
            }
            else -> null
        }
        js?.let { if (loaded) web.evaluateJavascript(it, null) else pendingJs = it }
        // Consume so a rotation does not re-trigger it.
        i.action = Intent.ACTION_MAIN
    }

    private fun looksLikeLink(s: String) = s.trim().let { it.startsWith("http://") || it.startsWith("https://") || it.startsWith("unishare:") }

    /** Copies a content:// stream to our cache and returns the path the Rust core can read. */
    private fun importToCache(uri: Uri): String? = try {
        val name = queryDisplayName(uri) ?: (uri.lastPathSegment?.substringAfterLast('/') ?: "shared.bin")
        val dir = File(cacheDir, "inbox").apply { mkdirs() }
        val out = File(dir, name.replace(Regex("[\\\\/:*?\"<>|]"), "_"))
        contentResolver.openInputStream(uri)?.use { input -> out.outputStream().use { input.copyTo(it) } }
        out.absolutePath
    } catch (_: Exception) {
        null
    }

    private fun importManyToCache(uris: List<Uri>): String? {
        if (uris.isEmpty()) return null
        if (uris.size == 1) return importToCache(uris[0])
        val dir = File(cacheDir, "inbox/shared-${System.currentTimeMillis()}").apply { mkdirs() }
        var n = 0
        for (u in uris) {
            try {
                val name = queryDisplayName(u) ?: "file-$n"
                contentResolver.openInputStream(u)?.use { input ->
                    File(dir, name.replace(Regex("[\\\\/:*?\"<>|]"), "_")).outputStream().use { input.copyTo(it) }
                }
                n++
            } catch (_: Exception) {
            }
        }
        return if (n > 0) dir.absolutePath else null
    }

    private fun queryDisplayName(uri: Uri): String? = try {
        contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
    } catch (_: Exception) {
        null
    }

    private fun jsStr(s: String): String =
        "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n").replace("\r", "").replace("</", "<\\/") + "\""

    @Suppress("DEPRECATION")
    private fun Intent.getParcelableUri(key: String): Uri? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) getParcelableExtra(key, Uri::class.java) else getParcelableExtra(key)

    @Suppress("DEPRECATION")
    private fun Intent.getParcelableUris(key: String): List<Uri> =
        (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) getParcelableArrayListExtra(key, Uri::class.java)
        else getParcelableArrayListExtra(key)) ?: emptyList()

    // ------------------------------------------------------------- permissions --

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }

    // ------------------------------------------------------------------- views --

    private fun buildSplash(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER
        setBackgroundColor(ContextCompat.getColor(this@MainActivity, R.color.bg))
        addView(ProgressBar(this@MainActivity).apply {
            indeterminateTintList = android.content.res.ColorStateList.valueOf(ContextCompat.getColor(this@MainActivity, R.color.accent))
        })
        addView(TextView(this@MainActivity).apply {
            text = getString(R.string.app_name)
            setTextColor(ContextCompat.getColor(this@MainActivity, R.color.fg))
            textSize = 16f
            setPadding(0, dp(16), 0, 0)
        })
    }

    private fun buildError(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER
        setPadding(dp(32), 0, dp(32), 0)
        setBackgroundColor(ContextCompat.getColor(this@MainActivity, R.color.bg))
        addView(TextView(this@MainActivity).apply {
            text = getString(R.string.engine_error)
            setTextColor(ContextCompat.getColor(this@MainActivity, R.color.fg))
            gravity = Gravity.CENTER
            textSize = 15f
        })
        addView(Button(this@MainActivity).apply {
            text = getString(R.string.retry)
            setTextColor(Color.WHITE)
            setOnClickListener { boot() }
        }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(16) })
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
