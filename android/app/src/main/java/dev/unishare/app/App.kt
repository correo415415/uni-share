package dev.unishare.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build
import android.provider.Settings
import java.io.File

class App : Application() {
    override fun onCreate() {
        super.onCreate()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
                    .apply { setShowBadge(false) }
            )
        }
    }

    /** Private app data: config.toml, history.sqlite, keys… */
    val dataDir: File get() = File(filesDir, "uni-share").apply { mkdirs() }

    /** Received files. App-specific external storage needs no runtime permission. */
    val downloadDir: File
        get() = (getExternalFilesDir(android.os.Environment.DIRECTORY_DOWNLOADS) ?: File(filesDir, "Download")).apply { mkdirs() }

    val deviceName: String
        get() = try {
            Settings.Global.getString(contentResolver, "device_name")
        } catch (_: Exception) {
            null
        }?.takeIf { it.isNotBlank() } ?: (Build.MANUFACTURER.replaceFirstChar { it.uppercase() } + " " + Build.MODEL)

    companion object {
        const val CHANNEL_ID = "engine"
    }
}
