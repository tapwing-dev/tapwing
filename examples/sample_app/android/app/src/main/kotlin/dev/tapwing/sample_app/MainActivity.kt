package dev.tapwing.sample_app

import android.Manifest
import android.content.pm.PackageManager
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import android.os.Handler
import android.os.Looper
import io.flutter.plugin.common.MethodChannel

/**
 * Hosts a tiny runtime-permission channel so the F5 native-lane flow has a REAL
 * Android system dialog to drive (`ActivityCompat.requestPermissions`).
 */
class MainActivity : FlutterActivity() {
    private var pendingResult: MethodChannel.Result? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL).setMethodCallHandler { call, result ->
            when (call.method) {
                "requestLocation" -> requestLocation(result)
                else -> result.notImplemented()
            }
        }
        // F7 app health sample: a real Java crash on the main thread.
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, HEALTH_CHANNEL).setMethodCallHandler { call, result ->
            when (call.method) {
                "crash" -> {
                    result.success(null)
                    Handler(Looper.getMainLooper()).post {
                        throw IllegalStateException("Tapwing F7 sample native crash")
                    }
                }
                else -> result.notImplemented()
            }
        }
    }

    private fun requestLocation(result: MethodChannel.Result) {
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_FINE_LOCATION) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            result.success(true)
            return
        }
        if (pendingResult != null) {
            result.error("busy", "A permission request is already in flight", null)
            return
        }
        pendingResult = result
        ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.ACCESS_FINE_LOCATION), REQUEST_LOCATION)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQUEST_LOCATION) {
            pendingResult?.success(grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED)
            pendingResult = null
        }
    }

    private companion object {
        const val CHANNEL = "dev.tapwing.sample_app/permissions"
        const val HEALTH_CHANNEL = "dev.tapwing.sample_app/health"
        const val REQUEST_LOCATION = 7710
    }
}
