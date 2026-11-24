import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

/// One currently running animation (or active scroll) whose visible area meets
/// the significance threshold, reported in `busy` diagnostics and used by
/// settle policy `v2`.
class SignificantAnimation {
  /// Creates a diagnostic record for a running animation.
  const SignificantAnimation({
    required this.widget,
    required this.fraction,
    required this.status,
    this.infinite = false,
  });

  /// Runtime type name of the animating widget (e.g. `FadeTransition`,
  /// `Scrollable`).
  final String widget;

  /// Visible area fraction of the screen (`0.0`–`1.0`).
  final double fraction;

  /// Current [AnimationStatus] (`forward` or `reverse`).
  final AnimationStatus status;

  /// Whether this animation has been running continuously with an unchanged
  /// semantics tree for at least [AnimationScanner.infiniteThreshold] and is
  /// therefore treated as an endless ambient animation (non-blocking).
  final bool infinite;

  /// Serializes to the wire format read by the host CLI's `--check-settle`.
  Map<String, Object?> toJson() => <String, Object?>{
    'widget': widget,
    'fraction': double.parse(fraction.toStringAsFixed(3)),
    'status': status.name,
    if (infinite) 'infinite': true,
  };
}

/// Read-only scanner that detects running "significant" animations and active
/// scrolls covering at least [significantFraction] (2 %) of the screen.
///
/// Inspects only the public framework widgets every route transition, dialog,
/// snackbar, hero flight, implicit animation, and scroll activity is built from:
/// - [AnimatedWidget] (`SlideTransition`, `ScaleTransition`, `AnimatedBuilder`,
///   `ListenableBuilder`, ...)
/// - [DualTransitionBuilder] (Material page transitions)
/// - [FadeTransition] / [SliverFadeTransition]
/// - [ImplicitlyAnimatedWidgetState] (`AnimatedContainer`, `AnimatedPadding`,
///   `AnimatedPositioned`, ...)
/// - [ScrollableState] (`position.isScrollingNotifier`: ballistic/driven scroll
///   tail during which Flutter sets `ScrollActivity.shouldIgnorePointer = true`)
///
/// Subtrees inside `TickerMode(enabled: false)` (covered routes) and
/// `Offstage(offstage: true)` are skipped so muted or hidden animations never
/// block settling.
///
/// Endless significant animations (e.g. a pulsing CTA button, shimmer skeleton,
/// animated background gradient, or marquee auto-scroll) that keep running with
/// an unchanged semantics tree for at least [infiniteThreshold] (1500 ms) are
/// marked `infinite: true` and stop blocking settling until they stop and
/// restart.
class AnimationScanner {
  /// Creates an animation scanner with an optional monotonic [clock]
  /// (defaults to a wall-clock [Stopwatch]).
  AnimationScanner({Duration Function()? clock})
    : _readNow = clock,
      _stopwatch = clock == null ? (Stopwatch()..start()) : null;

  /// Screen fraction above which a running animation blocks settling (2 %).
  static const double significantFraction = 0.02;

  /// Duration an animation may run continuously with an unchanged semantics
  /// tree before being classified as endless/non-blocking (1500 ms).
  ///
  /// Justification: standard Flutter/Material/Cupertino UI transitions complete
  /// in 150–600 ms (dialogs 150 ms, bottom sheets/snackbars 250 ms, Material
  /// page transitions 300–450 ms, Android Clamping flings ~300–600 ms, iOS
  /// Bouncing tails ~500–1000 ms). 1500 ms gives > 2.5× headroom over real
  /// transitions while halving worst-case first-encounter wait (vs the 3 s
  /// settle timeout) and reducing every subsequent settle on that screen to
  /// 3 frames (~48 ms).
  static const Duration infiniteThreshold = Duration(milliseconds: 1500);

  final Duration Function()? _readNow;
  final Stopwatch? _stopwatch;

  final Map<Object, Duration> _stableSince = Map<Object, Duration>.identity();
  final Set<Object> _knownInfinite = Set<Object>.identity();
  int? _lastTreeHash;

  Duration get _now => _readNow != null ? _readNow() : _stopwatch!.elapsed;

  /// Clears tracked animation identities (called when the scheduler reaches
  /// full `framesIdle`, guaranteeing stopped animations start fresh next time).
  void clearTracking() {
    _stableSince.clear();
    _knownInfinite.clear();
    _lastTreeHash = null;
  }

  /// Returns all running significant animations (including those classified as
  /// `infinite: true` for diagnostics).
  List<SignificantAnimation> scan({
    double threshold = significantFraction,
    int? treeHash,
    bool includeScrollables = true,
  }) => _walk(
    threshold: threshold,
    treeHash: treeHash,
    includeScrollables: includeScrollables,
  );

  /// Returns `true` if at least one non-infinite running animation covers at
  /// least [threshold] of the screen.
  bool hasSignificantAnimation({
    double threshold = significantFraction,
    int? treeHash,
    bool includeScrollables = true,
  }) => _walk(
    threshold: threshold,
    treeHash: treeHash,
    includeScrollables: includeScrollables,
  ).any((SignificantAnimation a) => !a.infinite);

  List<SignificantAnimation> _walk({
    required double threshold,
    required int? treeHash,
    required bool includeScrollables,
  }) {
    final Element? root = WidgetsBinding.instance.rootElement;
    final RenderView? view = RendererBinding.instance.renderViews.firstOrNull;
    final RenderBox? rootBox = view?.child;
    if (root == null || rootBox == null || !rootBox.hasSize) {
      clearTracking();
      return const <SignificantAnimation>[];
    }

    // Transform both the root screen rect and each widget's render box into
    // the same coordinate space so coverage fractions are accurate regardless
    // of transforms or device pixel ratio.
    final Rect screen = MatrixUtils.transformRect(
      rootBox.getTransformTo(null),
      Offset.zero & rootBox.size,
    );
    final double screenArea = screen.width * screen.height;
    if (screenArea <= 0) return const <SignificantAnimation>[];

    final Duration now = _now;
    final bool hashChanged =
        treeHash != null && _lastTreeHash != null && treeHash != _lastTreeHash;
    if (treeHash != null) {
      _lastTreeHash = treeHash;
    }

    final Set<Object> seenKeys = Set<Object>.identity();
    final List<
      ({Object key, String widget, double fraction, AnimationStatus status})
    >
    raw =
        <
          ({Object key, String widget, double fraction, AnimationStatus status})
        >[];

    void visit(Element element) {
      if (!element.mounted) return;
      final Widget widget = element.widget;
      if (widget is TickerMode && !widget.enabled) return;
      if (widget is Offstage && widget.offstage) return;

      // IndexedStack keeps inactive tab subtrees mounted without Offstage or
      // TickerMode(enabled: false); visit only the visible active child so
      // background-tab animations/shimmers never block settling.
      if (widget is IndexedStack) {
        final int? activeIndex = widget.index;
        if (activeIndex == null) return;
        int childIndex = 0;
        element.visitChildren((Element child) {
          if (childIndex == activeIndex) {
            visit(child);
          }
          childIndex++;
        });
        return;
      }

      // Skip SliverList/Grid keep-alive children parked in the keep-alive
      // bucket (layoutOffset == null), which are off-screen and would crash
      // `getTransformTo(null)`.
      final RenderObject? ro = element.renderObject;
      if (ro != null &&
          ro.parentData is SliverMultiBoxAdaptorParentData &&
          (ro.parentData! as SliverMultiBoxAdaptorParentData).layoutOffset ==
              null) {
        return;
      }

      // A scroll still moving (ballistic / driven / spring-back) counts as a
      // significant animation: its sub-pixel tail keeps the semantics hash
      // stable, yet the Scrollable ignores taps on its children until the
      // activity ends (`ScrollActivity.shouldIgnorePointer = true`).
      if (includeScrollables &&
          element is StatefulElement &&
          element.state is ScrollableState) {
        ScrollPosition? position;
        try {
          position = (element.state as ScrollableState).position;
        } on Object {
          position = null;
        }
        if (position != null && position.isScrollingNotifier.value) {
          final double? fraction = _visibleScreenFraction(
            element,
            screen,
            screenArea,
          );
          if (fraction != null && fraction >= threshold) {
            seenKeys.add(position);
            raw.add((
              key: position,
              widget: 'Scrollable',
              fraction: fraction,
              status: AnimationStatus.forward,
            ));
          }
        }
      }

      final Animation<Object?>? animation = _extractRunningAnimation(
        element,
        widget,
      );
      if (animation != null) {
        final double? fraction = _visibleScreenFraction(
          element,
          screen,
          screenArea,
        );
        if (fraction != null && fraction >= threshold) {
          // Key by Element (not Animation) so widgets using
          // `Tween(...).animate(controller)` inside `build()` retain their
          // endless-animation classification across setState rebuilds, while
          // still resetting as soon as `isAnimating` becomes false.
          seenKeys.add(element);
          raw.add((
            key: element,
            widget: widget.runtimeType.toString(),
            fraction: fraction,
            status: animation.status,
          ));
        }
      }
      element.visitChildren(visit);
    }

    root.visitChildren(visit);

    // Prune identities that stopped animating since the previous scan so
    // reused AnimationControllers or ScrollPositions start fresh next time.
    _stableSince.removeWhere((Object key, _) => !seenKeys.contains(key));
    _knownInfinite.removeWhere((Object key) => !seenKeys.contains(key));

    final List<SignificantAnimation> out = <SignificantAnimation>[];
    for (final item in raw) {
      bool isInfinite = _knownInfinite.contains(item.key);
      if (!isInfinite) {
        if (hashChanged) {
          _stableSince[item.key] = now;
        } else {
          final Duration firstSeen = _stableSince.putIfAbsent(
            item.key,
            () => now,
          );
          if (now - firstSeen >= infiniteThreshold) {
            _knownInfinite.add(item.key);
            isInfinite = true;
          }
        }
      }
      out.add(
        SignificantAnimation(
          widget: item.widget,
          fraction: item.fraction,
          status: item.status,
          infinite: isInfinite,
        ),
      );
    }
    return out;
  }

  static Animation<Object?>? _extractRunningAnimation(
    Element element,
    Widget widget,
  ) {
    Listenable? listenable = switch (widget) {
      final AnimatedWidget animated => animated.listenable,
      final DualTransitionBuilder dual => dual.animation,
      final FadeTransition fade => fade.opacity,
      final SliverFadeTransition sliverFade => sliverFade.opacity,
      _ => null,
    };
    if (listenable == null &&
        element is StatefulElement &&
        element.state is ImplicitlyAnimatedWidgetState) {
      listenable = (element.state as ImplicitlyAnimatedWidgetState).animation;
    }
    return listenable is Animation<Object?> && listenable.isAnimating
        ? listenable
        : null;
  }

  static double? _visibleScreenFraction(
    Element element,
    Rect screen,
    double screenArea,
  ) {
    final RenderObject? ro = element.findRenderObject();
    if (ro is! RenderBox || !ro.attached || !ro.hasSize) return null;
    final Rect transformed;
    try {
      transformed = MatrixUtils.transformRect(
        ro.getTransformTo(null),
        Offset.zero & ro.size,
      );
    } on Object {
      return null;
    }
    if (!transformed.isFinite) return null;
    final Rect rect = transformed.intersect(screen);
    if (!rect.isFinite || rect.width <= 0 || rect.height <= 0) return 0;
    return rect.width * rect.height / screenArea;
  }
}
