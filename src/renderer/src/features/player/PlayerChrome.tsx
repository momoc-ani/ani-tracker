import {
  ArrowLeft,
  Captions,
  Check,
  ExternalLink,
  ListVideo,
  LoaderCircle,
  Maximize,
  Minimize2,
  MonitorPlay,
  MoreVertical,
  Sparkles,
  Pause,
  PictureInPicture2,
  Play,
  Ratio,
  RotateCcw,
  RotateCw,
  Settings,
  SkipBack,
  SkipForward,
  Volume2,
  VolumeX,
  X
} from "lucide-react";
import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from "@/components/ui/dropdown-menu";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle
} from "@/components/ui/sheet";
import { Slider } from "@/components/ui/slider";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/cn";
import type { RemotePlaybackRequestMode, RemotePlaybackSubtitle } from "@shared/contracts";
import {
  PLAYER_SUBTITLE_SCALES,
  type PlayerAspectRatio,
  type PlayerFrameInterpolation,
  type PlayerSubtitleScale,
  type PlayerVideoEnhancement
} from "@shared/player-contract";
import { formatPlaybackTime } from "./player-ui-model";
import type { DesktopWindowDragHandlers } from "./use-desktop-window-drag";

const PLAYBACK_RATES = [0.5, 0.75, 1, 1.25, 1.5, 2] as const;

interface PlayerChromeProps {
  animeTitle: string;
  bufferedSeconds: number;
  buffering: boolean;
  canGoNext: boolean;
  canGoPrevious: boolean;
  currentTimeSeconds: number;
  durationSeconds: number;
  episodeLabel: string;
  externalPlayerLabel?: string;
  externalPlayerOpening?: boolean;
  fullscreen: boolean;
  mode?: RemotePlaybackRequestMode;
  muted: boolean;
  nativeWindowDrag?: boolean;
  onActivity: () => void;
  onChangeMode?: (mode: RemotePlaybackRequestMode) => void;
  onChangeRate: (rate: number) => void;
  onChangeSubtitle: (subtitleId?: string) => void;
  onChangeSubtitleScale: (subtitleScale: PlayerSubtitleScale) => void;
  onChangeVideoEnhancement?: (videoEnhancement: PlayerVideoEnhancement) => void;
  onChangeFrameInterpolation?: (frameInterpolation: PlayerFrameInterpolation) => void;
  onClose: () => void;
  onGoNext: () => void;
  onGoPrevious: () => void;
  onOpenExternalPlayer?: () => void;
  onOpenPlaylist: () => void;
  onPanelOpenChange: (open: boolean) => void;
  onSeek: (seconds: number) => void;
  onSetAspectRatio: (aspectRatio: PlayerAspectRatio) => void;
  onSetVolume: (volume: number) => void;
  onToggleFullscreen: () => void;
  onToggleMute: () => void;
  onTogglePictureInPicture: () => void;
  onTogglePlay: () => void;
  pictureInPicture: boolean;
  pictureInPictureAvailable?: boolean;
  playbackRate: number;
  playing: boolean;
  selectedSubtitleId?: string;
  statusBadges: string[];
  subtitleScale: PlayerSubtitleScale;
  subtitleScaleAvailable: boolean;
  videoEnhancement?: PlayerVideoEnhancement;
  videoEnhancementAvailable?: boolean;
  videoEnhancementDegraded?: boolean;
  frameInterpolation?: PlayerFrameInterpolation;
  frameInterpolationAvailable?: boolean;
  subtitles: RemotePlaybackSubtitle[];
  visible: boolean;
  volume: number;
  windowDragHandlers?: DesktopWindowDragHandlers;
}

/** 渲染适配桌面、横屏和手机竖屏的播放器控制层。 */
export function PlayerChrome(props: PlayerChromeProps) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const handleSettingsOpenChange = (open: boolean): void => {
    setSettingsOpen(open);
    props.onPanelOpenChange(open);
  };

  return (
    <TooltipProvider delayDuration={300}>
      <div
        aria-hidden={!props.visible}
        className={cn(
          "pointer-events-none absolute inset-0 z-20 flex flex-col justify-between transition-opacity duration-200",
          props.visible ? "opacity-100" : "pointer-events-none opacity-0"
        )}
        data-player-controls
        onPointerMove={props.onActivity}
        ref={(element) => element?.toggleAttribute("inert", !props.visible)}
      >
        <PlayerTopBar {...props} onOpenSettings={() => handleSettingsOpenChange(true)} />
        <PlayerCenterControls {...props} />
        <PlayerBottomBar {...props} onOpenSettings={() => handleSettingsOpenChange(true)} />
      </div>
      <PlayerSettingsSheet
        {...props}
        onOpenChange={handleSettingsOpenChange}
        open={settingsOpen}
      />
    </TooltipProvider>
  );
}

/** 渲染播放器标题、集数、切集和面板入口。 */
function PlayerTopBar(
  props: PlayerChromeProps & { onOpenSettings: () => void }
) {
  return (
    <header
      className="pointer-events-auto flex min-h-16 items-start gap-2 bg-black/55 pb-3 pl-[max(0.75rem,var(--safe-area-left))] pr-[max(0.75rem,var(--safe-area-right))] pt-[max(0.75rem,var(--safe-area-top))] text-white backdrop-blur-sm sm:pl-[max(1rem,var(--safe-area-left))] sm:pr-[max(1rem,var(--safe-area-right))]"
      data-player-drag-region={props.nativeWindowDrag ? "" : undefined}
      {...props.windowDragHandlers}
    >
      <PlayerIconButton label="关闭播放器" onClick={props.onClose}>
        <ArrowLeft />
      </PlayerIconButton>
      <div className="min-w-0 flex-1 self-center">
        <div className="flex min-w-0 items-center gap-2">
          <h1 className="truncate text-sm font-semibold sm:text-base">{props.animeTitle}</h1>
          <span className="shrink-0 text-xs text-white/75">{props.episodeLabel}</span>
        </div>
        <div className="player-desktop-status mt-1 flex flex-wrap gap-1.5">
          {props.statusBadges.map((label) => <Badge key={label}>{label}</Badge>)}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-0.5" data-player-no-drag>
        <PlayerIconButton className="player-top-secondary" disabled={!props.canGoPrevious} label="上一集" onClick={props.onGoPrevious}>
          <SkipBack />
        </PlayerIconButton>
        <PlayerIconButton className="player-top-secondary" disabled={!props.canGoNext} label="下一集" onClick={props.onGoNext}>
          <SkipForward />
        </PlayerIconButton>
        <PlayerIconButton className="player-top-secondary" label="播放列表" onClick={props.onOpenPlaylist}>
          <ListVideo />
        </PlayerIconButton>
        <PlayerIconButton className="player-mobile-settings player-top-settings" label="播放设置" onClick={props.onOpenSettings}>
          <Settings />
        </PlayerIconButton>
        <PlayerMoreMenu {...props} />
      </div>
    </header>
  );
}

/** 渲染快退、播放和快进的大尺寸操作。 */
function PlayerCenterControls(props: PlayerChromeProps) {
  return (
    <div className="pointer-events-none flex flex-1 items-center justify-center gap-5 sm:gap-8" data-player-no-drag>
      <PlayerIconButton className="player-center-skip pointer-events-auto bg-black/40 hover:bg-black/60 md:size-14" label="快退 10 秒" onClick={() => props.onSeek(props.currentTimeSeconds - 10)} size="media">
        <SeekSecondsIcon direction="backward" />
      </PlayerIconButton>
      <Button
        aria-label={props.buffering ? "正在缓冲" : props.playing ? "暂停" : "播放"}
        disabled={props.buffering}
        className="pointer-events-auto"
        onClick={props.onTogglePlay}
        size="media-large"
        variant="media-strong"
      >
        {props.buffering
          ? <LoaderCircle className="animate-spin" />
          : props.playing ? <Pause /> : <Play />}
      </Button>
      <PlayerIconButton className="player-center-skip pointer-events-auto bg-black/40 hover:bg-black/60 md:size-14" label="快进 10 秒" onClick={() => props.onSeek(props.currentTimeSeconds + 10)} size="media">
        <SeekSecondsIcon direction="forward" />
      </PlayerIconButton>
    </div>
  );
}

/** 渲染时间轴以及按视口收敛的底部控制栏。 */
function PlayerBottomBar(
  props: PlayerChromeProps & { onOpenSettings: () => void }
) {
  return (
    <footer
      className="pointer-events-auto bg-black/60 pb-[max(0.75rem,var(--safe-area-bottom))] pl-[max(0.75rem,var(--safe-area-left))] pr-[max(0.75rem,var(--safe-area-right))] pt-3 text-white backdrop-blur-sm sm:pl-[max(1rem,var(--safe-area-left))] sm:pr-[max(1rem,var(--safe-area-right))]"
      data-player-no-drag
    >
      <PlayerTimeline {...props} />
      <div className="mt-2 flex min-h-11 items-center justify-between gap-2">
        <div className="player-desktop-controls flex min-w-0 items-center gap-1">
          <PlayerIconButton label={props.playing ? "暂停" : "播放"} onClick={props.onTogglePlay}>
            {props.playing ? <Pause /> : <Play />}
          </PlayerIconButton>
          <PlayerIconButton disabled={!props.canGoPrevious} label="上一集" onClick={props.onGoPrevious}>
            <SkipBack />
          </PlayerIconButton>
          <PlayerIconButton disabled={!props.canGoNext} label="下一集" onClick={props.onGoNext}>
            <SkipForward />
          </PlayerIconButton>
          <span className="ml-1 shrink-0 text-xs tabular-nums text-white/80">
            {formatPlaybackTime(props.currentTimeSeconds)} / {formatPlaybackTime(props.durationSeconds)}
          </span>
          <PlayerIconButton label={props.muted ? "取消静音" : "静音"} onClick={props.onToggleMute}>
            {props.muted || props.volume === 0 ? <VolumeX /> : <Volume2 />}
          </PlayerIconButton>
          <Slider
            aria-label="音量"
            className="player-volume-slider w-24"
            max={1}
            min={0}
            onValueChange={([value]) => props.onSetVolume(value)}
            step={0.01}
            value={[props.muted ? 0 : props.volume]}
          />
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          <SubtitleMenu {...props} />
          <PlaybackRateMenu {...props} />
          <AspectRatioMenu {...props} />
          <VideoEnhancementMenu {...props} />
          <FrameInterpolationMenu {...props} />
          {props.mode && props.onChangeMode && <PlaybackModeMenu {...props} mode={props.mode} onChangeMode={props.onChangeMode} />}
          {props.pictureInPictureAvailable !== false && (
            <PlayerIconButton aria-pressed={props.pictureInPicture} label="画中画" onClick={props.onTogglePictureInPicture}>
              <PictureInPicture2 />
            </PlayerIconButton>
          )}
          <PlayerIconButton className="player-wide-control" label="播放列表" onClick={props.onOpenPlaylist}>
            <ListVideo />
          </PlayerIconButton>
          <PlayerIconButton label={props.fullscreen ? "退出全屏" : "全屏"} onClick={props.onToggleFullscreen}>
            {props.fullscreen ? <Minimize2 /> : <Maximize />}
          </PlayerIconButton>
          <PlayerIconButton className="player-bottom-settings player-mobile-settings" label="更多设置" onClick={props.onOpenSettings}>
            <MoreVertical />
          </PlayerIconButton>
        </div>
      </div>
    </footer>
  );
}

/** 渲染带缓冲进度和拖动预览的媒体时间轴。 */
function PlayerTimeline(props: PlayerChromeProps) {
  const [previewSeconds, setPreviewSeconds] = useState<number>();
  const duration = Math.max(props.durationSeconds, 1);
  const displayTime = previewSeconds ?? props.currentTimeSeconds;
  const bufferedPercent = Math.min(100, Math.max(0, props.bufferedSeconds / duration * 100));

  return (
    <div className="flex items-center gap-3">
      <span className="player-mobile-time w-10 shrink-0 text-right text-[11px] tabular-nums text-white/80">
        {formatPlaybackTime(displayTime)}
      </span>
      <div className="relative flex h-11 min-w-0 flex-1 items-center min-[901px]:h-6">
        <div className="pointer-events-none absolute inset-x-0 h-1.5 overflow-hidden rounded-full bg-white/20">
          <div className="h-full bg-white/35" style={{ width: `${bufferedPercent}%` }} />
        </div>
        <Slider
          aria-label="播放进度"
          aria-valuetext={`${formatPlaybackTime(displayTime)}，总时长 ${formatPlaybackTime(props.durationSeconds)}`}
          className="relative h-full"
          max={duration}
          min={0}
          onValueChange={([value]) => setPreviewSeconds(value)}
          onValueCommit={([value]) => {
            setPreviewSeconds(undefined);
            props.onSeek(value);
          }}
          step={0.1}
          value={[Math.min(displayTime, duration)]}
        />
      </div>
      <span className="player-mobile-time w-10 shrink-0 text-[11px] tabular-nums text-white/80">
        {formatPlaybackTime(props.durationSeconds)}
      </span>
    </div>
  );
}

/** 提供字幕开关与轨道选择。 */
function SubtitleMenu(props: PlayerChromeProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          aria-label="字幕"
          disabled={props.subtitles.length === 0 && !props.subtitleScaleAvailable}
          size="icon"
          variant="media"
        >
          <Captions />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>字幕</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          onValueChange={(value) => props.onChangeSubtitle(value === "off" ? undefined : value)}
          value={props.selectedSubtitleId ?? "off"}
        >
          <DropdownMenuRadioItem value="off">关闭字幕</DropdownMenuRadioItem>
          {props.subtitles.map((subtitle) => (
            <DropdownMenuRadioItem key={subtitle.id} value={subtitle.id}>
              {subtitle.label} · {subtitle.type.toUpperCase()}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        {props.subtitleScaleAvailable && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel>字幕大小</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              onValueChange={(value) => props.onChangeSubtitleScale(Number(value) as PlayerSubtitleScale)}
              value={String(props.subtitleScale)}
            >
              {PLAYER_SUBTITLE_SCALES.map((scale) => (
                <DropdownMenuRadioItem key={scale} value={String(scale)}>{scale}%</DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 提供固定倍速集合。 */
function PlaybackRateMenu(props: PlayerChromeProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="播放速度" size="icon" variant="media">
          <span className="text-xs font-semibold tabular-nums">{props.playbackRate === 1 ? "倍速" : `${props.playbackRate}x`}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>播放速度</DropdownMenuLabel>
        <DropdownMenuRadioGroup onValueChange={(value) => props.onChangeRate(Number(value))} value={String(props.playbackRate)}>
          {PLAYBACK_RATES.map((rate) => (
            <DropdownMenuRadioItem key={rate} value={String(rate)}>{rate}x</DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 提供画面比例选项。 */
function AspectRatioMenu(props: PlayerChromeProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="画面比例" className="player-wide-control" size="icon" variant="media"><Ratio /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>画面比例</DropdownMenuLabel>
        <DropdownMenuGroup>
          {(["default", "16:9", "4:3", "fit", "fill"] as PlayerAspectRatio[]).map((ratio) => (
            <DropdownMenuItem key={ratio} onSelect={() => props.onSetAspectRatio(ratio)}>
              {formatAspectRatio(ratio)}
            </DropdownMenuItem>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 仅在原生 GPU shader 可用时提供三档画质增强。 */
function VideoEnhancementMenu(props: PlayerChromeProps) {
  if (!props.videoEnhancementAvailable || !props.onChangeVideoEnhancement) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="画质增强" size="icon" variant="media"><Sparkles /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>画质增强</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          onValueChange={(value) => props.onChangeVideoEnhancement?.(value as PlayerVideoEnhancement)}
          value={props.videoEnhancement ?? "off"}
        >
          <DropdownMenuRadioItem value="off">关闭</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="balanced">均衡</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="clear">清晰</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
        {props.videoEnhancementDegraded && (
          <>
            <DropdownMenuSeparator />
            <p className="max-w-56 px-2 py-1.5 text-xs text-muted-foreground">
              已根据实时渲染性能自动降低档位
            </p>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 仅在实际模型运行时声明可用时提供补帧入口。 */
function FrameInterpolationMenu(props: PlayerChromeProps) {
  if (!props.frameInterpolationAvailable || !props.onChangeFrameInterpolation) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="实时补帧" size="icon" variant="media"><Sparkles /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>实时补帧</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          onValueChange={(value) => props.onChangeFrameInterpolation?.(value as PlayerFrameInterpolation)}
          value={props.frameInterpolation ?? "off"}
        >
          <DropdownMenuRadioItem value="off">关闭</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="rife-realtime">RIFE 实时</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 提供原文件与实时转码模式切换。 */
function PlaybackModeMenu(
  props: PlayerChromeProps & {
    mode: RemotePlaybackRequestMode;
    onChangeMode: (mode: RemotePlaybackRequestMode) => void;
  }
) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="播放模式" className="player-wide-control" size="icon" variant="media"><MonitorPlay /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>播放模式</DropdownMenuLabel>
        <DropdownMenuRadioGroup onValueChange={(value) => props.onChangeMode(value as RemotePlaybackRequestMode)} value={props.mode}>
          <DropdownMenuRadioItem value="direct">不转码</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="transcode">实时转码</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 收纳低频桌面操作。 */
function PlayerMoreMenu(props: PlayerChromeProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="更多操作" className="player-desktop-more" size="icon" variant="media"><MoreVertical /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>播放器</DropdownMenuLabel>
        <DropdownMenuGroup>
          {props.onOpenExternalPlayer && (
            <DropdownMenuItem disabled={props.externalPlayerOpening} onSelect={props.onOpenExternalPlayer}>
              <ExternalLink />
              {props.externalPlayerOpening ? "正在准备播放地址" : `用 ${props.externalPlayerLabel ?? "本机播放器"} 打开`}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={props.onOpenPlaylist}><ListVideo />播放列表</DropdownMenuItem>
          {props.pictureInPictureAvailable !== false && (
            <DropdownMenuItem onSelect={props.onTogglePictureInPicture}><PictureInPicture2 />画中画</DropdownMenuItem>
          )}
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuGroup>
          <DropdownMenuItem onSelect={props.onClose}><X />关闭播放器</DropdownMenuItem>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 手机端使用底部面板集中承载次要设置。 */
function PlayerSettingsSheet(
  props: PlayerChromeProps & { open: boolean; onOpenChange: (open: boolean) => void }
) {
  const onChangeMode = props.onChangeMode;
  return (
    <Sheet open={props.open} onOpenChange={props.onOpenChange}>
      <SheetContent className="max-h-[78svh] overflow-y-auto p-0" data-player-sheet side="bottom">
        <SheetHeader className="border-b px-4 py-3 pr-14 text-left">
          <SheetTitle>播放设置</SheetTitle>
          <SheetDescription>{props.animeTitle} · {props.episodeLabel}</SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-5 p-4">
          {props.mode && onChangeMode && <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">播放模式</h3>
            <ToggleGroup className="justify-start" onValueChange={(value) => value && onChangeMode(value as RemotePlaybackRequestMode)} type="single" value={props.mode} variant="outline">
              <ToggleGroupItem value="direct">不转码</ToggleGroupItem>
              <ToggleGroupItem value="transcode">实时转码</ToggleGroupItem>
            </ToggleGroup>
          </div>}
          {props.videoEnhancementAvailable && props.onChangeVideoEnhancement && (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">画质增强</h3>
              <ToggleGroup
                className="justify-start"
                onValueChange={(value) => value && props.onChangeVideoEnhancement?.(value as PlayerVideoEnhancement)}
                type="single"
                value={props.videoEnhancement ?? "off"}
                variant="outline"
              >
                <ToggleGroupItem value="off">关闭</ToggleGroupItem>
                <ToggleGroupItem value="balanced">均衡</ToggleGroupItem>
                <ToggleGroupItem value="clear">清晰</ToggleGroupItem>
              </ToggleGroup>
              {props.videoEnhancementDegraded && (
                <p className="text-xs text-muted-foreground">已根据实时渲染性能自动降低档位</p>
              )}
            </div>
          )}
          {props.frameInterpolationAvailable && props.onChangeFrameInterpolation && (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">实时补帧</h3>
              <ToggleGroup
                className="justify-start"
                onValueChange={(value) => value && props.onChangeFrameInterpolation?.(value as PlayerFrameInterpolation)}
                type="single"
                value={props.frameInterpolation ?? "off"}
                variant="outline"
              >
                <ToggleGroupItem value="off">关闭</ToggleGroupItem>
                <ToggleGroupItem value="rife-realtime">RIFE 实时</ToggleGroupItem>
              </ToggleGroup>
            </div>
          )}
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">播放速度</h3>
            <ToggleGroup className="flex-wrap justify-start" onValueChange={(value) => value && props.onChangeRate(Number(value))} type="single" value={String(props.playbackRate)} variant="outline">
              {PLAYBACK_RATES.map((rate) => <ToggleGroupItem key={rate} value={String(rate)}>{rate}x</ToggleGroupItem>)}
            </ToggleGroup>
          </div>
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">字幕</h3>
            {props.subtitles.length === 0 ? (
              <p className="text-sm text-muted-foreground">无可用文本字幕</p>
            ) : (
              <div className="flex flex-col gap-1">
                <Button className="justify-start" onClick={() => props.onChangeSubtitle(undefined)} variant={props.selectedSubtitleId ? "ghost" : "secondary"}>
                  {!props.selectedSubtitleId && <Check data-icon="inline-start" />}关闭字幕
                </Button>
                {props.subtitles.map((subtitle) => (
                  <Button className="justify-start" key={subtitle.id} onClick={() => props.onChangeSubtitle(subtitle.id)} variant={props.selectedSubtitleId === subtitle.id ? "secondary" : "ghost"}>
                    {props.selectedSubtitleId === subtitle.id && <Check data-icon="inline-start" />}
                    {subtitle.label} · {subtitle.type.toUpperCase()}
                  </Button>
                ))}
              </div>
            )}
          </div>
          {props.subtitleScaleAvailable && (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">字幕大小</h3>
              <ToggleGroup
                className="flex-wrap justify-start"
                onValueChange={(value) => value && props.onChangeSubtitleScale(Number(value) as PlayerSubtitleScale)}
                type="single"
                value={String(props.subtitleScale)}
                variant="outline"
              >
                {PLAYER_SUBTITLE_SCALES.map((scale) => (
                  <ToggleGroupItem key={scale} value={String(scale)}>{scale}%</ToggleGroupItem>
                ))}
              </ToggleGroup>
            </div>
          )}
          {props.onOpenExternalPlayer && (
            <Button disabled={props.externalPlayerOpening} onClick={props.onOpenExternalPlayer} variant="outline">
              <ExternalLink data-icon="inline-start" />
              {props.externalPlayerOpening ? "正在准备播放地址" : `用 ${props.externalPlayerLabel ?? "本机播放器"} 打开`}
            </Button>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}

/** 用旋转方向和秒数共同表达十秒快退或快进。 */
function SeekSecondsIcon({ direction }: { direction: "backward" | "forward" }) {
  const Icon = direction === "backward" ? RotateCcw : RotateCw;
  return (
    <span aria-hidden="true" className="relative flex size-7 items-center justify-center">
      <Icon />
      <span className="absolute text-[8px] font-bold leading-none">10</span>
    </span>
  );
}

/** 渲染带 Tooltip 的媒体图标按钮。 */
function PlayerIconButton({
  children,
  className,
  label,
  size = "icon",
  ...props
}: React.ComponentProps<typeof Button> & { label: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button aria-label={label} className={className} size={size} variant="media" {...props}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

/** 将画面比例标识转换为中文。 */
function formatAspectRatio(ratio: PlayerAspectRatio): string {
  return ({ default: "默认", "16:9": "16:9", "4:3": "4:3", fill: "填充", fit: "适应窗口", custom: "自定义" })[ratio];
}
