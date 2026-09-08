package dev.unishare.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

/**
 * Watches the engine's `/api/state` for pending LAN offers and shows one heads-up
 * notification per offer with **Aceptar / Rechazar** actions, so an incoming transfer can be
 * answered from the lock screen or while another app is in front. The WebView shows the same
 * offer card when the app is open; notifications are cancelled once the offer disappears.
 *
 * Polling (3 s) runs only while the foreground service is alive; it is cheap (loopback, ~1 KB).
 */
class OfferWatcher(private val ctx: Context) {

    private val io = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private val shown = HashSet<String>()
    private var running = false

    private val tick = object : Runnable {
        override fun run() {
            if (!running) return
            io.execute { poll() }
            main.postDelayed(this, PERIOD_MS)
        }
    }

    fun start() {
        if (running) return
        running = true
        ensureChannel()
        main.post(tick)
    }

    fun stop() {
        running = false
        main.removeCallbacks(tick)
        val nm = ctx.getSystemService(NotificationManager::class.java)
        for (id in shown) nm.cancel(TAG, id.hashCode())
        shown.clear()
        io.shutdown()
    }

    private fun poll() {
        val port = EngineService.port()
        if (port <= 0) return
        val body = try {
            (URL("http://127.0.0.1:$port/api/state").openConnection() as HttpURLConnection).run {
                connectTimeout = 1500; readTimeout = 1500
                inputStream.bufferedReader().use { it.readText() }
            }
        } catch (_: Exception) {
            return
        }
        val pending = try {
            JSONObject(body).optJSONArray("pending")
        } catch (_: Exception) {
            null
        } ?: return
        val nm = ctx.getSystemService(NotificationManager::class.java)
        val current = HashSet<String>()
        for (i in 0 until pending.length()) {
            val o = pending.getJSONObject(i)
            val id = o.optString("transfer_id")
            if (id.isEmpty()) continue
            current.add(id)
            if (shown.add(id)) nm.notify(TAG, id.hashCode(), build(o))
        }
        // Offers answered elsewhere (WebView, another device timing out) → drop their notification.
        val gone = shown.filter { it !in current }
        for (id in gone) {
            nm.cancel(TAG, id.hashCode())
            shown.remove(id)
        }
    }

    private fun build(o: JSONObject): Notification {
        val id = o.getString("transfer_id")
        val sender = o.optString("sender", "?")
        val name = o.optString("name", "")
        val size = human(o.optLong("total_size", 0))
        val files = o.optJSONArray("files")?.length() ?: 1
        val peer = o.optString("peer", "")
        val fp = o.optString("sender_fingerprint", "").take(19)

        val open = PendingIntent.getActivity(
            ctx, id.hashCode(), Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val accept = PendingIntent.getBroadcast(
            ctx, (id + "a").hashCode(), Intent(ctx, OfferReceiver::class.java).setAction(OfferReceiver.ACTION_ACCEPT).putExtra(OfferReceiver.EXTRA_ID, id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val reject = PendingIntent.getBroadcast(
            ctx, (id + "r").hashCode(), Intent(ctx, OfferReceiver::class.java).setAction(OfferReceiver.ACTION_REJECT).putExtra(OfferReceiver.EXTRA_ID, id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        return NotificationCompat.Builder(ctx, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("$sender quiere enviarte $name")
            .setContentText("$size · $files archivo(s) · desde $peer")
            .setStyle(NotificationCompat.BigTextStyle().bigText("$size · $files archivo(s) · desde $peer\nHuella del remitente: $fp"))
            .setContentIntent(open)
            .addAction(0, "Aceptar", accept)
            .addAction(0, "Rechazar", reject)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setAutoCancel(false)
            .setOnlyAlertOnce(true)
            .build()
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = ctx.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, ctx.getString(R.string.notif_offers_channel), NotificationManager.IMPORTANCE_HIGH)
                    .apply { description = ctx.getString(R.string.notif_offers_desc) }
            )
        }
    }

    private fun human(b: Long): String {
        val u = arrayOf("B", "KiB", "MiB", "GiB", "TiB")
        var v = b.toDouble(); var i = 0
        while (v >= 1024 && i < u.size - 1) { v /= 1024; i++ }
        return if (i == 0) "$b B" else String.format(java.util.Locale.ROOT, "%.1f %s", v, u[i])
    }

    companion object {
        const val CHANNEL_ID = "offers"
        private const val TAG = "offer"
        private const val PERIOD_MS = 3000L
    }
}
