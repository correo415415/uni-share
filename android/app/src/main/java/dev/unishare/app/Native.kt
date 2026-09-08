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
}
