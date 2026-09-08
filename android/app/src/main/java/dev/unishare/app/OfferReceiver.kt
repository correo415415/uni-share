package dev.unishare.app

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/** Notification actions → engine REST call (`/api/offers/{id}/accept|reject`) on loopback. */
class OfferReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra(EXTRA_ID) ?: return
        val verb = if (intent.action == ACTION_ACCEPT) "accept" else "reject"
        val port = EngineService.port()
        val pending = goAsync()
        thread(name = "offer-$verb") {
            try {
                if (port > 0) {
                    (URL("http://127.0.0.1:$port/api/offers/$id/$verb").openConnection() as HttpURLConnection).run {
                        requestMethod = "POST"
                        connectTimeout = 2000; readTimeout = 4000
                        setRequestProperty("Content-Type", "application/json")
                        doOutput = true
                        outputStream.use { it.write("{}".toByteArray()) }
                        responseCode
                        disconnect()
                    }
                }
            } catch (_: Exception) {
            } finally {
                context.getSystemService(NotificationManager::class.java).cancel("offer", id.hashCode())
                pending.finish()
            }
        }
    }

    companion object {
        const val ACTION_ACCEPT = "dev.unishare.app.OFFER_ACCEPT"
        const val ACTION_REJECT = "dev.unishare.app.OFFER_REJECT"
        const val EXTRA_ID = "id"
    }
}
