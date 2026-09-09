package dev.unishare.app

/** JNI bridge to the Rust core (`libuni_share.so`, see `src/android.rs`). */
object Native {
    init {
        System.loadLibrary("uni_share")
    }

    /** Starts engine + local web GUI. Returns the loopback port, or a negative value on error. */
    external fun start(dataDir: String, downloadDir: String, deviceName: String): Int

    /** Graceful shutdown (idempotent). */
    external fun stop()

    /** Current port, 0 when not running. */
    external fun port(): Int

    external fun version(): String

    /** Appends a shell-side line to the shared log buffer (shown in Ajustes → Registro). `level` = android.util.Log.* */
    external fun log(level: Int, tag: String, message: String)

    /** logcat + shared buffer in one call; safe before the library is loaded (falls back to logcat only). */
    fun logBoth(level: Int, tag: String, message: String) {
        android.util.Log.println(level, tag, message)
        try { log(level, tag, message) } catch (_: Throwable) { }
    }
}
