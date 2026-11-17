package top.passionke.supone_tunnel

// Author: kejiqing

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import kotlin.concurrent.thread

class MainActivity : AppCompatActivity() {
    private lateinit var runtime: CoreRuntime
    private lateinit var statusText: TextView
    private lateinit var logText: TextView
    private lateinit var subscriptionUrl: EditText

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        runtime = CoreRuntime(this)
        statusText = findViewById(R.id.statusText)
        logText = findViewById(R.id.logText)
        subscriptionUrl = findViewById(R.id.subscriptionUrl)
        subscriptionUrl.setText(runtime.savedSubscription())

        findViewById<Button>(R.id.btnStart).setOnClickListener { onStartCore() }
        findViewById<Button>(R.id.btnStop).setOnClickListener { onStopCore() }
        findViewById<Button>(R.id.btnYoutube).setOnClickListener {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://www.youtube.com")))
        }
        refreshStatus()
        appendLog("Android 壳：mihomo + 全局 http_proxy（不移植桌面 MCP/托盘）")
        // adb: am start -n top.passionke.supone_tunnel/.MainActivity --ez autostart true
        if (intent?.getBooleanExtra("autostart", false) == true) {
            onStartCore()
        }
    }

    private fun onStartCore() {
        val url = subscriptionUrl.text?.toString().orEmpty()
        appendLog("导入并启动…")
        thread {
            try {
                val extract = runtime.extractBinaryIfNeeded()
                val imported = runtime.importSubscription(url)
                val started = runtime.start()
                runOnUiThread {
                    appendLog("$extract\n$imported\n$started")
                    refreshStatus()
                }
            } catch (err: Exception) {
                runOnUiThread {
                    appendLog("失败: ${err.message}")
                    refreshStatus()
                }
            }
        }
    }

    private fun onStopCore() {
        thread {
            val msg = try {
                runtime.stop()
            } catch (err: Exception) {
                "停止失败: ${err.message}"
            }
            runOnUiThread {
                appendLog(msg)
                refreshStatus()
            }
        }
    }

    private fun refreshStatus() {
        statusText.text = runtime.statusLine()
    }

    private fun appendLog(msg: String) {
        val old = logText.text?.toString().orEmpty()
        logText.text = if (old.isEmpty()) msg else "$old\n$msg"
    }
}
