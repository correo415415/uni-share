package dev.unishare.app

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the Rust engine alive while the app is in the background so LAN peers can still
 * discover us and in-flight transfers finish. Starting is synchronous (the JNI call blocks
 * until the local web server is bound), so we do it on a worker thread and cache the port.
 */
class EngineService : Service() {

    /** mDNS needs multicast; many vendors drop multicast packets unless the app holds a lock. */
    private var multicast: WifiManager.MulticastLock? = null
    /** Incoming-offer notifications with Aceptar/Rechazar (see OfferWatcher). */
    private var offers: OfferWatcher? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        try {
            val wifi = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
            multicast = wifi.createMulticastLock("uni-share-mdns").apply {
                setReferenceCounted(false)
                acquire()
            }
        } catch (_: Exception) {
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground()
        if (intent?.action == ACTION_STOP) {
            Native.stop()
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        ensureStarted(applicationContext)
        if (offers == null) offers = OfferWatcher(applicationContext).also { it.start() }
        return START_STICKY
    }

    override fun onDestroy() {
        offers?.stop()
        offers = null
        try {
            multicast?.takeIf { it.isHeld }?.release()
        } catch (_: Exception) {
        }
        multicast = null
        // The activity may still be showing the WebView; the engine itself only stops on ACTION_STOP.
        super.onDestroy()
    }

    private fun startForeground() {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, EngineService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val n: Notification = NotificationCompat.Builder(this, App.CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(getString(R.string.notif_text))
            .setContentIntent(open)
            .addAction(0, "Detener", stop)
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(NOTIF_ID, n)
        }
    }

    companion object {
        const val ACTION_STOP = "dev.unishare.app.STOP"
        private const val NOTIF_ID = 1

        @Volatile
        private var cachedPort: Int = 0
        private val lock = Any()

        /** Blocking: starts the engine if needed and returns the loopback port (<= 0 on error). */
        fun ensureStarted(ctx: Context): Int {
            synchronized(lock) {
                val running = Native.port()
                if (running > 0) {
                    cachedPort = running
                    return running
                }
                val app = ctx.applicationContext as App
                val p = Native.start(app.engineDir.absolutePath, app.downloadDir.absolutePath, app.deviceName)
                cachedPort = p
                return p
            }
        }

        fun port(): Int = Native.port().takeIf { it > 0 } ?: cachedPort

        fun start(ctx: Context) {
            ContextCompat.startForegroundService(ctx, Intent(ctx, EngineService::class.java))
        }

        fun stop(ctx: Context) {
            ctx.startService(Intent(ctx, EngineService::class.java).setAction(ACTION_STOP))
        }
    }
}
