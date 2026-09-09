package dev.unishare.app

import android.content.pm.ActivityInfo
import android.os.Bundle
import com.google.zxing.BarcodeFormat
import com.google.zxing.DecodeHintType
import com.journeyapps.barcodescanner.CaptureActivity
import com.journeyapps.barcodescanner.DecoratedBarcodeView
import com.journeyapps.barcodescanner.DefaultDecoderFactory

/**
 * Portrait-only QR scanner. The stock zxing `CaptureActivity` follows the sensor orientation
 * (landscape on most phones) and uses the fast decoder; uni-share tickets are dense QR codes
 * (version 15-25), so we lock portrait, enable TRY_HARDER and let the decoder also read
 * inverted codes (dark theme screens).
 */
class ScanActivity : CaptureActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
    }

    override fun initializeContent(): DecoratedBarcodeView {
        val view = super.initializeContent()
        val hints = mapOf<DecodeHintType, Any>(
            DecodeHintType.TRY_HARDER to true,
            DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
        )
        // scanType 2 = mixed: alternate normal and inverted frames (QR shown on a dark-theme screen).
        view.barcodeView.decoderFactory = DefaultDecoderFactory(listOf(BarcodeFormat.QR_CODE), hints, null, 2)
        view.setStatusText(getString(R.string.scan_prompt))
        return view
    }
}
