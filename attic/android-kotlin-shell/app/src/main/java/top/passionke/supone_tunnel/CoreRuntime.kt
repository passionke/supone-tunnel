package top.passionke.supone_tunnel

// Author: kejiqing
// Android 壳：只负责 mihomo 启停 + 全局 http_proxy。
// 不移植桌面托盘 / MCP / TUN；本验收路径面向可 root 的 android-x86。

import android.content.Context
import android.provider.Settings
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.net.HttpURLConnection
import java.net.URL
import java.nio.charset.StandardCharsets

class CoreRuntime(private val context: Context) {
    private val rootDir: File = File(context.filesDir, "supone")
    private val binDir: File = File(rootDir, "bin")
    private val workDir: File = File(rootDir, "work")
    private val mihomo: File = File(binDir, "mihomo")
    private val config: File = File(workDir, "config.yaml")
    private val logFile: File = File(workDir, "mihomo.log")

    fun ensureLayout() {
        binDir.mkdirs()
        workDir.mkdirs()
    }

    fun extractBinaryIfNeeded(): String {
        ensureLayout()
        if (mihomo.isFile && mihomo.length() > 1_000_000L) {
            mihomo.setExecutable(true, false)
            return "内核已就绪 ${mihomo.length()} bytes"
        }
        val assetName = "mihomo-android-amd64"
        context.assets.open(assetName).use { input ->
            mihomo.outputStream().use { output -> input.copyTo(output) }
        }
        mihomo.setExecutable(true, false)
        return "已解压内核 ${mihomo.length()} bytes"
    }

    fun importSubscription(url: String): String {
        ensureLayout()
        val body = httpGet(url.trim())
        if (!body.contains("proxies:")) {
            throw IllegalStateException("订阅不像 Clash YAML（缺少 proxies）")
        }
        // GEOIP 依赖 MMDB；虚机常拉不到。规则里去掉 GEOIP，DOMAIN-SUFFIX 仍覆盖 YouTube。
        val filtered = body.lineSequence()
            .filterNot { it.trim().startsWith("- GEOIP,") }
            .joinToString("\n")
        val yaml = buildString {
            appendLine("mixed-port: 7890")
            appendLine("allow-lan: true")
            appendLine("bind-address: \"*\"")
            appendLine("mode: rule")
            appendLine("log-level: info")
            appendLine("external-controller: 127.0.0.1:9090")
            appendLine("ipv6: false")
            appendLine("find-process-mode: off")
            appendLine("dns:")
            appendLine("  enable: true")
            appendLine("  enhanced-mode: fake-ip")
            appendLine("  fake-ip-range: 198.18.0.1/16")
            appendLine("  nameserver:")
            appendLine("    - 223.5.5.5")
            appendLine("    - 119.29.120.120")
            appendLine(filtered)
            appendLine()
        }
        config.writeText(yaml, StandardCharsets.UTF_8)
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_SUB_URL, url.trim())
            .apply()
        return "已写入配置 ${config.length()} bytes"
    }

    fun savedSubscription(): String =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(KEY_SUB_URL, DEFAULT_SUB)
            ?: DEFAULT_SUB

    fun start(): String {
        extractBinaryIfNeeded()
        if (!config.isFile) {
            throw IllegalStateException("尚无配置，请先导入订阅")
        }
        stopQuiet()
        logFile.writeText("")
        val cmd = arrayOf(
            "/system/bin/sh",
            "-c",
            "cd ${workDir.absolutePath} && nohup ${mihomo.absolutePath} -d ${workDir.absolutePath} -f ${config.absolutePath} > ${logFile.absolutePath} 2>&1 & echo \$!",
        )
        val pid = runCapture(cmd).trim()
        Thread.sleep(1200)
        setHttpProxy("127.0.0.1:7890")
        val alive = isRunning()
        val tail = logFile.takeIf { it.isFile }?.readText()?.takeLast(800) ?: ""
        if (!alive) {
            throw IllegalStateException("内核未起来\n$tail")
        }
        return "已启动 pid=$pid\n$tail"
    }

    fun stop(): String {
        stopQuiet()
        clearHttpProxy()
        return "已停止，并清除全局 http_proxy"
    }

    fun isRunning(): Boolean {
        val out = runCapture(arrayOf("/system/bin/sh", "-c", "ps -A | grep mihomo | grep -v grep || true"))
        return out.contains("mihomo")
    }

    fun statusLine(): String {
        val run = if (isRunning()) "运行中" else "未运行"
        val proxy = try {
            Settings.Global.getString(context.contentResolver, "http_proxy") ?: "(空)"
        } catch (err: Exception) {
            "读失败:${err.message}"
        }
        return "状态: $run · http_proxy=$proxy"
    }

    private fun stopQuiet() {
        runCapture(
            arrayOf(
                "/system/bin/sh",
                "-c",
                "pkill -9 -f ${mihomo.absolutePath} 2>/dev/null; killall mihomo 2>/dev/null; true",
            ),
        )
    }

    private fun setHttpProxy(value: String) {
        // WRITE_SECURE_SETTINGS（deploy 脚本 pm grant）或 root su。
        try {
            Settings.Global.putString(context.contentResolver, "http_proxy", value)
        } catch (_: Exception) {
        }
        runStatus(arrayOf("/system/bin/su", "0", "settings", "put", "global", "http_proxy", value))
        runStatus(arrayOf("/system/xbin/su", "0", "settings", "put", "global", "http_proxy", value))
    }

    private fun clearHttpProxy() {
        try {
            Settings.Global.putString(context.contentResolver, "http_proxy", ":0")
        } catch (_: Exception) {
        }
        runStatus(arrayOf("/system/bin/su", "0", "settings", "put", "global", "http_proxy", ":0"))
        runStatus(arrayOf("/system/xbin/su", "0", "settings", "put", "global", "http_proxy", ":0"))
    }

    private fun httpGet(url: String): String {
        val conn = (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 30_000
            readTimeout = 60_000
            requestMethod = "GET"
            setRequestProperty("User-Agent", "supone-tunnel-android")
        }
        conn.inputStream.bufferedReader(StandardCharsets.UTF_8).use { return it.readText() }
    }

    private fun runCapture(cmd: Array<String>): String {
        val p = ProcessBuilder(*cmd).redirectErrorStream(true).start()
        val text = BufferedReader(InputStreamReader(p.inputStream, StandardCharsets.UTF_8)).readText()
        p.waitFor()
        return text
    }

    private fun runStatus(cmd: Array<String>): Int {
        return try {
            ProcessBuilder(*cmd).redirectErrorStream(true).start().waitFor()
        } catch (_: Exception) {
            1
        }
    }

    companion object {
        private const val PREFS = "supone"
        private const val KEY_SUB_URL = "subscription_url"
        // No hardcoded Hub token — set via prefs / UI. Author: kejiqing
        const val DEFAULT_SUB = ""
    }
}
