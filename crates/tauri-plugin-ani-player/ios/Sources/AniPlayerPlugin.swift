import Foundation
import SwiftUI
import Tauri
import UIKit

private struct DispatchArgs: Decodable {
    let commandJson: String
}

private enum AniPlayerPluginError: LocalizedError {
    case invalidCommand(String)
    case unavailable(String)

    var errorDescription: String? {
        switch self {
        case .invalidCommand(let message), .unavailable(let message): message
        }
    }
}

/** 将 Rust PlayerTransport 连接到 iOS MobileVLCKit SwiftUI 播放页。 */
final class AniPlayerPlugin: Plugin, UIAdaptivePresentationControllerDelegate {
    private var controller: MobileVLCPlayerController?
    private var hostingController: UIHostingController<PlayerScreen>?
    private var activeSessionID: String?
    private var activeSource: [String: Any]?
    private var sequence: UInt64 = 0
    private var backgroundObserver: NSObjectProtocol?
    private var foregroundObserver: NSObjectProtocol?

    /** 注册应用前后台通知并保持同一原生播放控制器。 */
    override init() {
        super.init()
        backgroundObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.didEnterBackgroundNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.controller?.enterBackground()
        }
        foregroundObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.willEnterForegroundNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.controller?.becomeActive()
        }
    }

    /** 插件释放时移除通知并停止仍在运行的媒体。 */
    deinit {
        if let backgroundObserver { NotificationCenter.default.removeObserver(backgroundObserver) }
        if let foregroundObserver { NotificationCenter.default.removeObserver(foregroundObserver) }
        let playerController = controller
        let playerHost = hostingController
        DispatchQueue.main.async {
            playerController?.close()
            playerHost?.dismiss(animated: false)
        }
    }

    /** 返回 iOS 内置 MobileVLCKit 的稳定能力集合。 */
    @objc public func capabilities(_ invoke: Invoke) {
        invoke.resolve(["capabilitiesJson": Self.encode(Self.capabilitiesPayload())])
    }

    /** 执行 Rust 已校验并解析真实路径的播放器命令。 */
    @objc public func dispatch(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(DispatchArgs.self)
        guard
            let data = args.commandJson.data(using: .utf8),
            let command = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            throw AniPlayerPluginError.invalidCommand("播放器命令不是有效 JSON")
        }
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            do {
                let result = try self.execute(command)
                invoke.resolve(["resultJson": Self.encode(result)])
            } catch {
                NSLog("AniPlayerPlugin command failed: %@", error.localizedDescription)
                invoke.reject(
                    "iOS 播放器命令失败",
                    code: "player_command_failed",
                    error: error
                )
            }
        }
    }

    /** 返回当前原生播放器快照；页面尚未打开时返回空。 */
    @objc public func snapshot(_ invoke: Invoke) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            let snapshotJson = self.snapshotPayload().map(Self.encode)
            let value: Any = snapshotJson ?? NSNull()
            invoke.resolve(["snapshotJson": value])
        }
    }

    /** 幂等关闭原生播放页并清除受控会话。 */
    @objc public func shutdown(_ invoke: Invoke) {
        DispatchQueue.main.async { [weak self] in
            self?.closePlayer(animated: false)
            invoke.resolve(["stopped": true])
        }
    }

    /** 用户通过系统交互关闭页面时同步释放 MobileVLCKit。 */
    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        closePlayer(animated: false)
    }

    /** 将动作映射到当前 MobileVLCKit 控制器。 */
    private func execute(_ command: [String: Any]) throws -> [String: Any] {
        let commandID = try Self.requiredString(command, key: "commandId")
        let sessionID = try Self.requiredString(command, key: "sessionId")
        let action = try Self.requiredString(command, key: "type")
        if action == "load" {
            try launch(command, sessionID: sessionID)
            return Self.accepted(commandID)
        }
        guard activeSessionID == sessionID, let controller else {
            return Self.rejected(commandID, code: "resource-unavailable", message: "播放器会话已切换")
        }

        switch action {
        case "play": controller.play()
        case "pause": controller.pause()
        case "seek":
            controller.seek(to: Self.milliseconds(command["positionSeconds"] as? Double))
        case "set-volume":
            let volume = Int32(((command["volume"] as? Double) ?? 0) * 100)
            controller.setVolume(volume)
        case "set-muted": controller.setMuted((command["muted"] as? Bool) == true)
        case "set-rate": controller.setPlaybackRate(Float((command["rate"] as? Double) ?? 1))
        case "select-audio-track":
            guard let trackID = Int32((command["trackId"] as? String) ?? "") else {
                return Self.rejected(commandID, code: "unsupported", message: "音轨标识无效")
            }
            controller.selectAudioTrack(trackID)
        case "select-subtitle-track":
            let trackID = (command["trackId"] as? String).flatMap { Int32($0) }
            controller.selectSubtitleTrack(trackID)
        case "set-subtitle-scale":
            controller.setSubtitleScale((command["subtitleScale"] as? Int) ?? 100)
        case "set-aspect-ratio":
            guard let ratio = Self.aspectRatio(command) else {
                return Self.rejected(commandID, code: "unsupported", message: "iOS 不支持该画面比例")
            }
            controller.selectAspectRatio(ratio)
        case "set-fullscreen":
            setFullscreen((command["fullscreen"] as? Bool) == true)
        case "set-picture-in-picture":
            return Self.rejected(commandID, code: "unsupported", message: "iOS 画中画尚未启用")
        case "set-frame-interpolation":
            return Self.rejected(commandID, code: "unsupported", message: "iOS 播放器暂不支持模型补帧")
        case "set-hdr":
            return Self.rejected(commandID, code: "unsupported", message: "iOS 播放器暂不支持 HDR 输出")
        case "previous-item": controller.previousEpisode()
        case "next-item": controller.nextEpisode()
        case "retry": controller.retry()
        case "close": closePlayer(animated: true)
        default:
            return Self.rejected(commandID, code: "unsupported", message: "当前播放器不支持该命令")
        }
        return Self.accepted(commandID)
    }

    /** 从受控来源创建播放请求并展示原生页面。 */
    private func launch(_ command: [String: Any], sessionID: String) throws {
        guard let source = command["source"] as? [String: Any] else {
            throw AniPlayerPluginError.invalidCommand("播放器加载命令缺少媒体来源")
        }
        let taskID = try Self.requiredString(source, key: "taskId")
        let title = try Self.requiredString(source, key: "title")
        let mediaURL = try Self.mediaURL(Self.requiredString(source, key: "uri"))
        let presentation = PlayerLaunchParser.parsePresentation(source, fallbackTitle: title)
        let fileIndex = source["fileIndex"] as? Int
        let episodeID = fileIndex.map { "\(taskID):\($0)" } ?? taskID
        let subtitles = (source["subtitles"] as? [[String: Any]] ?? []).enumerated().compactMap {
            index, value -> PlayerSubtitle? in
            guard
                let uri = value["uri"] as? String,
                let url = try? Self.mediaURL(uri)
            else { return nil }
            return PlayerSubtitle(
                id: (value["id"] as? String) ?? "subtitle-\(index)",
                label: (value["label"] as? String) ?? "字幕 \(index + 1)",
                url: url,
                language: value["language"] as? String,
                isDefault: (value["default"] as? Bool) ?? (index == 0)
            )
        }
        let durationMilliseconds = Self.milliseconds(source["durationSeconds"] as? Double)
        let episode = PlayerEpisode(
            id: episodeID,
            title: title,
            episodeLabel: title,
            mediaURL: mediaURL,
            durationMilliseconds: durationMilliseconds,
            subtitles: subtitles
        )
        let request = PlayerLaunchRequest(
            sessionID: sessionID,
            animeTitle: presentation.animeTitle,
            synopsis: presentation.synopsis,
            artworkURL: presentation.artworkURL,
            episodes: [episode],
            activeIndex: 0,
            startPositionMilliseconds: Self.milliseconds(command["startPositionSeconds"] as? Double),
            autoplay: true
        )
        let playerController = controller ?? MobileVLCPlayerController()
        controller = playerController
        playerController.initialize(request)
        activeSessionID = sessionID
        activeSource = source
        sequence = 0
        try presentPlayerIfNeeded(playerController)
        NSLog(
            "AniPlayerPlugin native player launched session=%@ artwork=%@",
            sessionID,
            presentation.artworkURL == nil ? "false" : "true"
        )
    }

    /** 在当前 Tauri iOS 窗口上全屏展示 SwiftUI 播放页。 */
    private func presentPlayerIfNeeded(_ controller: MobileVLCPlayerController) throws {
        if let hostingController, hostingController.presentingViewController != nil {
            return
        }
        guard let presenter = Self.topViewController() else {
            throw AniPlayerPluginError.unavailable("找不到可承载播放器的 iOS 窗口")
        }
        let host = UIHostingController(
            rootView: PlayerScreen(controller: controller) { [weak self] in
                self?.closePlayer(animated: true)
            }
        )
        host.modalPresentationStyle = .fullScreen
        host.isModalInPresentation = true
        hostingController = host
        presenter.present(host, animated: true) {
            host.presentationController?.delegate = self
        }
    }

    /** 停止媒体、关闭页面并清除真实路径引用。 */
    private func closePlayer(animated: Bool) {
        controller?.close()
        let host = hostingController
        hostingController = nil
        controller = nil
        activeSessionID = nil
        activeSource = nil
        sequence = 0
        if host?.presentingViewController != nil {
            host?.dismiss(animated: animated)
        }
    }

    /** 把 SwiftUI 状态归一为跨平台播放器快照。 */
    private func snapshotPayload() -> [String: Any]? {
        guard
            let controller,
            let sessionID = activeSessionID,
            let source = activeSource,
            controller.snapshot.sessionID == sessionID
        else { return nil }
        let snapshot = controller.snapshot
        sequence &+= 1
        let taskID = (source["taskId"] as? String) ?? ""
        let fileIndex = source["fileIndex"] as? Int
        let itemID = fileIndex.map { "\(taskID):\($0)" } ?? taskID
        var item: [String: Any] = [
            "id": itemID,
            "taskId": taskID,
            "title": (source["title"] as? String) ?? ""
        ]
        if let fileIndex { item["fileIndex"] = fileIndex }
        if let duration = source["durationSeconds"] as? Double { item["durationSeconds"] = duration }
        var payload: [String: Any] = [
            "sessionId": sessionID,
            "sequence": sequence,
            "backend": "libvlc",
            "platform": "ios",
            "status": Self.statusValue(snapshot.status),
            "capabilities": Self.capabilitiesPayload(),
            "source": source,
            "playlist": ["items": [item], "activeItemId": itemID],
            "positionSeconds": Double(snapshot.positionMilliseconds) / 1_000,
            "durationSeconds": Double(snapshot.durationMilliseconds) / 1_000,
            "bufferedSeconds": 0,
            "volume": Double(snapshot.volume) / 100,
            "muted": snapshot.muted,
            "playbackRate": Double(snapshot.playbackRate),
            "audioTracks": Self.trackPayload(snapshot.audioTracks, kind: "audio"),
            "subtitleTracks": Self.trackPayload(snapshot.subtitleTracks, kind: "subtitle"),
            "subtitleScale": snapshot.subtitleScale,
            "videoEnhancement": "off",
            "videoEnhancementDegraded": false,
            "frameInterpolation": "off",
            "hdr": "off",
            "enhancementDiagnostics": [
                "pipeline": "libvlc",
                "droppedFrames": 0,
                "hdrCapabilities": ["sourceHdr": false, "rendererHdr": false, "displayHdr": false]
            ],
            "aspectRatio": Self.aspectRatioValue(snapshot.aspectRatio),
            "fullscreen": Self.isLandscape,
            "pictureInPicture": false
        ]
        if let message = snapshot.errorMessage {
            payload["error"] = [
                "code": "decoder",
                "message": message,
                "recoverable": true,
                "recoveryActions": ["retry", "close"]
            ]
        }
        return payload
    }

    /** iOS 16+ 请求目标方向，iOS 15 保留系统手动旋转。 */
    private func setFullscreen(_ fullscreen: Bool) {
        guard let scene = Self.activeWindowScene else { return }
        if #available(iOS 16.0, *) {
            let preferences = UIWindowScene.GeometryPreferences.iOS(
                interfaceOrientations: fullscreen ? .landscape : .portrait
            )
            scene.requestGeometryUpdate(preferences) { error in
                NSLog("AniPlayerPlugin orientation update failed: %@", error.localizedDescription)
            }
        }
        UIViewController.attemptRotationToDeviceOrientation()
    }

    /** 读取非空字符串字段。 */
    private static func requiredString(_ object: [String: Any], key: String) throws -> String {
        guard let value = object[key] as? String, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw AniPlayerPluginError.invalidCommand("播放器字段 \(key) 无效")
        }
        return value
    }

    /** 将网络 URI 或绝对沙盒路径转换为 URL。 */
    private static func mediaURL(_ value: String) throws -> URL {
        if let url = URL(string: value), url.scheme != nil { return url }
        guard value.hasPrefix("/") else {
            throw AniPlayerPluginError.invalidCommand("媒体路径不是受支持的绝对路径")
        }
        return URL(fileURLWithPath: value)
    }

    /** 将有限秒数安全换算为毫秒。 */
    private static func milliseconds(_ seconds: Double?) -> Int64 {
        guard let seconds, seconds.isFinite, seconds > 0 else { return 0 }
        return Int64(min(seconds * 1_000, Double(Int64.max)))
    }

    /** 将公共比例枚举映射到 iOS 离散比例。 */
    private static func aspectRatio(_ command: [String: Any]) -> PlayerAspectRatio? {
        let kind = (command["aspectRatio"] as? String) ?? "default"
        let value = command["value"] as? String
        switch kind == "custom" ? value : kind {
        case "default", "fit":
            return .automatic
        case "16:9":
            return .widescreen
        case "4:3":
            return .standard
        default:
            return nil
        }
    }

    /** 构造成功命令响应。 */
    private static func accepted(_ commandID: String) -> [String: Any] {
        ["commandId": commandID, "accepted": true]
    }

    /** 构造稳定错误码的拒绝响应。 */
    private static func rejected(_ commandID: String, code: String, message: String) -> [String: Any] {
        [
            "commandId": commandID,
            "accepted": false,
            "error": [
                "code": code,
                "message": message,
                "recoverable": code != "unsupported",
                "recoveryActions": ["close"]
            ]
        ]
    }

    /** 返回 iOS MobileVLCKit 的能力字典。 */
    private static func capabilitiesPayload() -> [String: Any] {
        [
            "backend": "libvlc",
            "platform": "ios",
            "availability": "available",
            "canSeek": true,
            "canSetVolume": true,
            "canMute": true,
            "playbackRates": [0.5, 0.75, 1.0, 1.25, 1.5, 2.0],
            "supportsAudioTracks": true,
            "supportsSubtitleTracks": true,
            "supportsSubtitleScale": true,
            "supportsVideoEnhancement": false,
            "supportsFrameInterpolation": false,
            "supportsModelEnhancement": false,
            "supportsAspectRatio": true,
            "supportsFullscreen": true,
            "supportsPictureInPicture": false,
            "supportsPlaylistNavigation": true,
            "supportsDirectPlayback": true,
            "supportsTranscodingFallback": false,
            "supportsHdr": false
        ]
    }

    /** 将平台轨道转换为公共轨道结构。 */
    private static func trackPayload(_ tracks: [PlayerTrack], kind: String) -> [[String: Any]] {
        tracks.map { track in
            [
                "id": String(track.id),
                "kind": kind,
                "label": track.label,
                "selected": track.selected
            ]
        }
    }

    /** 返回跨语言稳定的播放状态值。 */
    private static func statusValue(_ status: PlayerStatus) -> String {
        switch status {
        case .idle: "idle"
        case .loading: "loading"
        case .ready: "ready"
        case .playing: "playing"
        case .paused: "paused"
        case .buffering: "buffering"
        case .ended: "ended"
        case .error: "error"
        }
    }

    /** 返回跨语言稳定的画面比例值。 */
    private static func aspectRatioValue(_ aspectRatio: PlayerAspectRatio) -> String {
        switch aspectRatio {
        case .automatic: "default"
        case .widescreen: "16:9"
        case .standard: "4:3"
        }
    }

    /** 将桥接对象编码为 Rust 可解码的 JSON。 */
    private static func encode(_ object: Any) -> String {
        guard
            JSONSerialization.isValidJSONObject(object),
            let data = try? JSONSerialization.data(withJSONObject: object),
            let value = String(data: data, encoding: .utf8)
        else { return "{}" }
        return value
    }

    private static var activeWindowScene: UIWindowScene? {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive }
    }

    private static var isLandscape: Bool {
        activeWindowScene?.interfaceOrientation.isLandscape == true
    }

    /** 查找当前可展示全屏播放器的最上层控制器。 */
    private static func topViewController() -> UIViewController? {
        let root = activeWindowScene?.windows.first(where: \.isKeyWindow)?.rootViewController
        var current = root
        while let presented = current?.presentedViewController {
            current = presented
        }
        return current
    }
}

@_cdecl("init_plugin_ani_player")
/** 向 Tauri iOS 注册播放器插件实例。 */
func initPlugin() -> Plugin {
    AniPlayerPlugin()
}
