#define GL_SILENCE_DEPRECATION
#import <AppKit/AppKit.h>
#import <OpenGL/OpenGL.h>
#import <dispatch/dispatch.h>
#import <stdatomic.h>

typedef struct AniMpvMacSurface AniMpvMacSurface;

@interface AniMpvOpenGLView : NSOpenGLView {
 @private
  AniMpvMacSurface *_aniSurface;
}

@property(nonatomic, assign) AniMpvMacSurface *aniSurface;

@end

struct AniMpvMacSurface {
  NSOpenGLView *view;
  NSOpenGLContext *context;
  _Atomic(int32_t) backing_width;
  _Atomic(int32_t) backing_height;
};

static void ani_mpv_update_backing_size(AniMpvOpenGLView *view) {
  AniMpvMacSurface *surface = view.aniSurface;
  if (surface == NULL) {
    return;
  }
  NSRect backing = [view convertRectToBacking:view.bounds];
  atomic_store_explicit(&surface->backing_width,
                        (int32_t)backing.size.width,
                        memory_order_release);
  atomic_store_explicit(&surface->backing_height,
                        (int32_t)backing.size.height,
                        memory_order_release);
}

@implementation AniMpvOpenGLView

@synthesize aniSurface = _aniSurface;

- (void)reshape {
  [super reshape];
  ani_mpv_update_backing_size(self);
}

- (void)setFrameSize:(NSSize)newSize {
  [super setFrameSize:newSize];
  ani_mpv_update_backing_size(self);
}

- (void)viewDidMoveToSuperview {
  [super viewDidMoveToSuperview];
  if (self.superview != nil) {
    self.frame = self.superview.bounds;
  }
  ani_mpv_update_backing_size(self);
}

- (void)viewDidMoveToWindow {
  [super viewDidMoveToWindow];
  ani_mpv_update_backing_size(self);
}

- (void)viewDidChangeBackingProperties {
  [super viewDidChangeBackingProperties];
  ani_mpv_update_backing_size(self);
}

@end

typedef struct AniMpvSurfaceCreateRequest {
  NSWindow *window;
  AniMpvMacSurface *surface;
} AniMpvSurfaceCreateRequest;

static void ani_mpv_create_surface_on_main(void *opaque) {
  AniMpvSurfaceCreateRequest *request = opaque;
  NSView *parent = request->window.contentView;
  if (parent == nil) {
    return;
  }
  [parent layoutSubtreeIfNeeded];
  NSOpenGLPixelFormatAttribute attributes[] = {
      NSOpenGLPFAOpenGLProfile,
      NSOpenGLProfileVersion3_2Core,
      NSOpenGLPFAAccelerated,
      NSOpenGLPFADoubleBuffer,
      NSOpenGLPFAAllowOfflineRenderers,
      0,
  };
  NSOpenGLPixelFormat *format =
      [[NSOpenGLPixelFormat alloc] initWithAttributes:attributes];
  if (format == nil) {
    return;
  }
  AniMpvMacSurface *surface = calloc(1, sizeof(AniMpvMacSurface));
  if (surface == NULL) {
    [format release];
    return;
  }
  AniMpvOpenGLView *view =
      [[AniMpvOpenGLView alloc] initWithFrame:parent.bounds
                                 pixelFormat:format];
  [format release];
  if (view == nil) {
    free(surface);
    return;
  }
  view.aniSurface = surface;
  surface->view = view;
  view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
  view.wantsBestResolutionOpenGLSurface = YES;
  view.wantsExtendedDynamicRangeOpenGLSurface = YES;
  [parent addSubview:view];
  view.frame = parent.bounds;
  [parent layoutSubtreeIfNeeded];
  [view prepareOpenGL];

  NSOpenGLContext *context = [[view openGLContext] retain];
  if (context == nil) {
    view.aniSurface = NULL;
    [view removeFromSuperview];
    [view release];
    free(surface);
    return;
  }
  GLint swap_interval = 1;
  [context setValues:&swap_interval forParameter:NSOpenGLContextParameterSwapInterval];

  surface->context = context;
  ani_mpv_update_backing_size(view);
  request->surface = surface;
}

void *ani_mpv_macos_surface_create(void *parent_window) {
  if (parent_window == NULL) {
    return NULL;
  }
  AniMpvSurfaceCreateRequest request = {
      .window = (NSWindow *)parent_window,
      .surface = NULL,
  };
  if ([NSThread isMainThread]) {
    ani_mpv_create_surface_on_main(&request);
  } else {
    dispatch_sync_f(dispatch_get_main_queue(), &request,
                    ani_mpv_create_surface_on_main);
  }
  return request.surface;
}

void *ani_mpv_macos_surface_cgl_context(void *opaque) {
  AniMpvMacSurface *surface = opaque;
  if (surface == NULL || surface->context == nil) {
    return NULL;
  }
  return [surface->context CGLContextObj];
}

int ani_mpv_macos_surface_backing_size(void *opaque, int *width, int *height) {
  AniMpvMacSurface *surface = opaque;
  if (surface == NULL || width == NULL || height == NULL) {
    return 10005;
  }
  *width = atomic_load_explicit(&surface->backing_width,
                                memory_order_acquire);
  *height = atomic_load_explicit(&surface->backing_height,
                                 memory_order_acquire);
  return *width > 0 && *height > 0 ? 0 : 10005;
}

static void ani_mpv_destroy_surface_on_main(void *opaque) {
  AniMpvMacSurface *surface = opaque;
  if (surface == NULL) {
    return;
  }
  ((AniMpvOpenGLView *)surface->view).aniSurface = NULL;
  [surface->context clearDrawable];
  [surface->view clearGLContext];
  [surface->view removeFromSuperview];
  [surface->context release];
  [surface->view release];
  free(surface);
}

void ani_mpv_macos_surface_destroy(void *opaque) {
  if (opaque == NULL) {
    return;
  }
  if ([NSThread isMainThread]) {
    ani_mpv_destroy_surface_on_main(opaque);
  } else {
    dispatch_sync_f(dispatch_get_main_queue(), opaque,
                    ani_mpv_destroy_surface_on_main);
  }
}
