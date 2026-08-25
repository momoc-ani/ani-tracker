package dev.ani.tracker.player

import android.app.Activity
import android.net.Uri
import android.util.Log
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import dev.ani.tracker.android.player.NativePlayerBridge
import dev.ani.tracker.android.player.PlayerEpisode
import dev.ani.tracker.android.player.PlayerLaunchContract
import dev.ani.tracker.android.player.PlayerLaunchRequest
import dev.ani.tracker.android.player.PlayerSubtitle
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

@InvokeArg
class DispatchArgs {
    lateinit var commandJson: String
}

/** 将 Rust PlayerTransport 连接到 Android libVLC Activity。 */
@TauriPlugin
class AniPlayerPlugin(private val activity: Activity) : Plugin(activity) {
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "ani-player-plugin")
    }
    private val sequence = AtomicLong(0L)
    @Volatile private var activeSessionId: String? = null
    @Volatile private var activeSource: JSONObject? = null

    /** 记录插件加载，不在 WebView 中注册原生文件命令。 */
    override fun load(webView: WebView) {
        Log.i(LOG_TAG, "Tauri Android player plugin loaded")
    }

    /** 返回 Android 内置 libVLC 的稳定能力集合。 */
    @Command
    fun capabilities(invoke: Invoke) {
        invoke.resolve(JSObject().put("capabilitiesJson", capabilitiesJson().toString()))
    }

    /** 执行 Rust 已校验并解析真实路径的播放器命令。 */
    @Command
    fun dispatch(invoke: Invoke) {
        val args = try {
            invoke.parseArgs(DispatchArgs::class.java)
        } catch (error: Exception) {
            invoke.reject("播放器命令参数无效", "invalid_player_command", error)
            return
        }
        executor.execute {
            try {
                val command = JSONObject(args.commandJson)
                val result = if (command.optString("type") == "load") {
                    launch(command)
                } else {
                    dispatchToActivity(command)
                }
                invoke.resolve(JSObject().put("resultJson", result.toString()))
            } catch (error: Exception) {
                Log.e(LOG_TAG, "Android player command failed", error)
                invoke.reject("Android 播放器命令失败", "player_command_failed", error)
            }
        }
    }

    /** 返回当前 Activity 快照；尚未打开播放器时返回空。 */
    @Command
    fun snapshot(invoke: Invoke) {
        executor.execute {
            try {
                val raw = NativePlayerBridge.snapshot().getOrThrow()
                val snapshot = raw?.let(::buildSnapshot)
                invoke.resolve(JSObject().put("snapshotJson", snapshot?.toString()))
            } catch (error: Exception) {
                Log.e(LOG_TAG, "Android player snapshot failed", error)
                invoke.reject("Android 播放器状态读取失败", "player_snapshot_failed", error)
            }
        }
    }

    /** 幂等关闭 Activity 并清除原生会话引用。 */
    @Command
    fun shutdown(invoke: Invoke) {
        executor.execute {
            try {
                NativePlayerBridge.shutdown().getOrThrow()
                activeSessionId = null
                activeSource = null
                invoke.resolve(JSObject().put("stopped", true))
            } catch (error: Exception) {
                Log.e(LOG_TAG, "Android player shutdown failed", error)
                invoke.reject("Android 播放器关闭失败", "player_shutdown_failed", error)
            }
        }
    }

    /** 插件销毁时释放串行执行器，Activity 生命周期由系统管理。 */
    override fun onDestroy() {
        executor.shutdownNow()
    }

    /** 从受控来源创建 Intent 并启动原生播放器。 */
    private fun launch(command: JSONObject): JSONObject {
        val commandId = command.getString("commandId")
        val sessionId = command.getString("sessionId")
        val source = command.getJSONObject("source")
        val mediaUri = normalizeUri(source.getString("uri"))
        val subtitles = source.optJSONArray("subtitles").toObjectList().mapIndexed { index, subtitle ->
            PlayerSubtitle(
                id = subtitle.optString("id", "subtitle-$index"),
                label = subtitle.optString("label", "字幕 ${index + 1}"),
                uri = normalizeUri(subtitle.getString("uri")),
                language = subtitle.optString("language").takeIf(String::isNotBlank),
                isDefault = subtitle.optBoolean("default", index == 0)
            )
        }
        val fileIndex = source.optInt("fileIndex", -1).takeIf { it >= 0 }
        val episodeId = buildString {
            append(source.getString("taskId"))
            fileIndex?.let { append(":").append(it) }
        }
        val request = PlayerLaunchRequest(
            sessionId = sessionId,
            animeTitle = source.optString("animeTitle").takeIf(String::isNotBlank)
                ?: source.getString("title"),
            description = source.optString("description"),
            artworkUri = source.optString("artworkUri").takeIf(String::isNotBlank),
            episodes = listOf(
                PlayerEpisode(
                    id = episodeId,
                    title = source.getString("title"),
                    episodeLabel = source.getString("title"),
                    uri = mediaUri,
                    durationMillis = (source.optDouble("durationSeconds", 0.0) * 1_000.0).toLong(),
                    subtitles = subtitles
                )
            ),
            activeIndex = 0,
            startPositionMillis = (command.optDouble("startPositionSeconds", 0.0) * 1_000.0).toLong(),
            autoplay = true
        )
        activity.runOnUiThread {
            activity.startActivity(PlayerLaunchContract.createIntent(activity, request))
        }
        activeSessionId = sessionId
        activeSource = JSONObject(source.toString())
        sequence.set(0L)
        Log.i(LOG_TAG, "Android native player launched session=$sessionId, artwork=${request.artworkUri != null}")
        return accepted(commandId)
    }

    /** 将非加载动作转发到当前 PlayerActivity。 */
    private fun dispatchToActivity(command: JSONObject): JSONObject {
        val commandId = command.getString("commandId")
        if (activeSessionId != command.optString("sessionId")) {
            return rejected(commandId, "resource-unavailable", "播放器会话已切换")
        }
        if (command.optString("type") == "set-picture-in-picture") {
            return rejected(commandId, "unsupported", "Android 画中画尚未启用")
        }
        if (command.optString("type") == "set-frame-interpolation") {
            return rejected(commandId, "unsupported", "Android 播放器暂不支持模型补帧")
        }
        if (command.optString("type") == "set-hdr") {
            return rejected(commandId, "unsupported", "Android 播放器暂不支持 HDR 输出")
        }
        val accepted = NativePlayerBridge.dispatch(command).getOrThrow()
        if (!accepted) return rejected(commandId, "unsupported", "当前播放器不支持该命令")
        if (command.optString("type") == "close") {
            activeSessionId = null
            activeSource = null
        }
        return accepted(commandId)
    }

    /** 把 Activity 状态归一为跨平台播放器快照。 */
    private fun buildSnapshot(raw: JSONObject): JSONObject? {
        val sessionId = activeSessionId ?: return null
        if (raw.optString("sessionId") != sessionId) return null
        val source = activeSource ?: return null
        val errorMessage = raw.optString("errorMessage").takeIf(String::isNotBlank)
        val sourceCopy = JSONObject(source.toString())
        val itemId = buildString {
            append(sourceCopy.optString("taskId"))
            if (sourceCopy.has("fileIndex")) append(":").append(sourceCopy.optInt("fileIndex"))
        }
        return JSONObject().apply {
            put("sessionId", sessionId)
            put("sequence", sequence.incrementAndGet())
            put("backend", "libvlc")
            put("platform", "android")
            put("status", raw.optString("status", "idle"))
            put("capabilities", capabilitiesJson())
            put("source", sourceCopy)
            put("playlist", JSONObject().apply {
                put("items", JSONArray().put(JSONObject().apply {
                    put("id", itemId)
                    put("taskId", sourceCopy.optString("taskId"))
                    if (sourceCopy.has("fileIndex")) put("fileIndex", sourceCopy.optInt("fileIndex"))
                    put("title", sourceCopy.optString("title"))
                    if (sourceCopy.has("durationSeconds")) {
                        put("durationSeconds", sourceCopy.optDouble("durationSeconds"))
                    }
                }))
                put("activeItemId", itemId)
            })
            put("positionSeconds", raw.optDouble("positionSeconds", 0.0))
            put("durationSeconds", raw.optDouble("durationSeconds", 0.0))
            put("bufferedSeconds", 0.0)
            put("volume", raw.optDouble("volume", 0.7))
            put("muted", raw.optBoolean("muted"))
            put("playbackRate", raw.optDouble("playbackRate", 1.0))
            put("audioTracks", normalizeTracks(raw.optJSONArray("audioTracks"), "audio"))
            put("subtitleTracks", normalizeTracks(raw.optJSONArray("subtitleTracks"), "subtitle"))
            put("subtitleScale", raw.optInt("subtitleScale", 100))
            put("videoEnhancement", "off")
            put("videoEnhancementDegraded", false)
            put("frameInterpolation", "off")
            put("hdr", "off")
            put("enhancementDiagnostics", JSONObject().apply {
                put("pipeline", "libvlc")
                put("droppedFrames", 0)
                put("hdrCapabilities", JSONObject().apply {
                    put("sourceHdr", false)
                    put("rendererHdr", false)
                    put("displayHdr", false)
                })
            })
            val aspectRatio = raw.opt("aspectRatio")
                ?.takeUnless { it == JSONObject.NULL }
                ?.toString()
                ?.takeIf(String::isNotBlank)
                ?: "default"
            put("aspectRatio", aspectRatio)
            put("fullscreen", raw.optBoolean("fullscreen"))
            put("pictureInPicture", false)
            errorMessage?.let {
                put("error", JSONObject().apply {
                    put("code", "decoder")
                    put("message", it)
                    put("recoverable", true)
                    put("recoveryActions", JSONArray().put("retry").put("close"))
                })
            }
        }
    }

    /** 返回 Android libVLC 的能力字典。 */
    private fun capabilitiesJson(): JSONObject = JSONObject().apply {
        put("backend", "libvlc")
        put("platform", "android")
        put("availability", "available")
        put("canSeek", true)
        put("canSetVolume", true)
        put("canMute", true)
        put("playbackRates", JSONArray(listOf(0.5, 0.75, 1.0, 1.25, 1.5, 2.0)))
        put("supportsAudioTracks", true)
        put("supportsSubtitleTracks", true)
        put("supportsSubtitleScale", true)
        put("supportsVideoEnhancement", false)
        put("supportsFrameInterpolation", false)
        put("supportsModelEnhancement", false)
        put("supportsAspectRatio", true)
        put("supportsFullscreen", true)
        put("supportsPictureInPicture", false)
        put("supportsPlaylistNavigation", true)
        put("supportsDirectPlayback", true)
        put("supportsTranscodingFallback", false)
        put("supportsHdr", false)
    }

    /** 将平台轨道转换为公共轨道结构。 */
    private fun normalizeTracks(tracks: JSONArray?, kind: String): JSONArray = JSONArray().apply {
        tracks.toObjectList().forEach { track ->
            put(JSONObject().apply {
                put("id", track.optString("id"))
                put("kind", kind)
                put("label", track.optString("label"))
                put("selected", track.optBoolean("selected"))
            })
        }
    }

    /** 将绝对文件路径转换为 libVLC 可识别的 file URI。 */
    private fun normalizeUri(value: String): String {
        val parsed = Uri.parse(value)
        return if (parsed.scheme.isNullOrBlank()) Uri.fromFile(File(value)).toString() else value
    }

    /** 构造成功命令响应。 */
    private fun accepted(commandId: String): JSONObject = JSONObject().apply {
        put("commandId", commandId)
        put("accepted", true)
    }

    /** 构造稳定错误码的拒绝响应。 */
    private fun rejected(commandId: String, code: String, message: String): JSONObject = JSONObject().apply {
        put("commandId", commandId)
        put("accepted", false)
        put("error", JSONObject().apply {
            put("code", code)
            put("message", message)
            put("recoverable", code != "unsupported")
            put("recoveryActions", JSONArray().put("close"))
        })
    }

    /** 将可空 JSON 数组安全转换为对象列表。 */
    private fun JSONArray?.toObjectList(): List<JSONObject> {
        if (this == null) return emptyList()
        return buildList {
            for (index in 0 until length()) optJSONObject(index)?.let(::add)
        }
    }

    companion object {
        private const val LOG_TAG = "AniPlayerPlugin"
    }
}
