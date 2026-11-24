import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

/// One currently running animation whose visible area meets the significance
/// threshold, reported in `busy` diagnostics and used by settle policy `v2`.
class SignificantAnimation {
  /// Creates a diagnostic record for a running animation.
  const SignificantAnimation({
    required this.widget,
    required this.fraction,
    required this.status,
  });

  /// Runtime type name of the animating widget (e.g. `FadeTransition`).
  final String widget;

  /// Visible area fraction of the screen (`0.0`–`1.0`).
  final double fraction;

  /// Current [AnimationStatus] (`forward` or `reverse`).
  final AnimationStatus status;

  /// Serializes to the wire format read by the host CLI's `--check-settle`.
  Map<String, Object?> toJson() => <String, Object?>{
    'widget': widget,
    'fraction': double.parse(fraction.toStringAsFixed(3)),
    'status': status.name,
  };
}

/// Read-only scanner that detects running "significant" animations covering
/// at least [significantFraction] (2 %) of the screen.
///
/// Inspects only the public framework widgets every route transition, dialog,
/// snackbar, hero flight, and implicit animation is built from:
/// - [AnimatedWidget] (`SlideTransition`, `ScaleTransition`, `AnimatedBuilder`,
///   `ListenableBuilder`, ...)
/// - [DualTransitionBuilder] (Material page transitions)
/// - [FadeTransition] / [SliverFadeTransition]
/// - [ImplicitlyAnimatedWidgetState] (`AnimatedContainer`, `AnimatedPadding`,
///   `AnimatedPositioned`, ...)
///
/// Subtrees inside `TickerMode(enabled: false)` (covered routes) and
/// `Offstage(offstage: true)` are skipped so muted or hidden animations never
/// block settling.
///
/// Important: every check rescans the live element tree rather than caching a
/// blocking `Animation`. When a route is covered mid-transition, its
/// `TickerMode` becomes disabled while `Animation.isAnimating` remains `true`
/// forever; rescanning immediately stops counting that muted subtree.
class AnimationScanner {
  /// Creates a stateless animation scanner.
  const AnimationScanner();

  /// Screen fraction above which a running animation blocks settling (2 %).
  static const double significantFraction = 0.02;

  /// Returns all running animations covering at least [threshold] of the screen.
  List<SignificantAnimation> scan({double threshold = significantFraction}) =>
      _walk(threshold: threshold, firstOnly: false);

  /// Returns `true` as soon as one running animation covers at least
  /// [threshold] of the screen.
  bool hasSignificantAnimation({double threshold = significantFraction}) =>
      _walk(threshold: threshold, firstOnly: true).isNotEmpty;

  List<SignificantAnimation> _walk({
    required double threshold,
    required bool firstOnly,
  }) {
    final Element? root = WidgetsBinding.instance.rootElement;
    final RenderView? view = RendererBinding.instance.renderViews.firstOrNull;
    final RenderBox? rootBox = view?.child;
    if (root == null || rootBox == null || !rootBox.hasSize) {
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

    final List<SignificantAnimation> out = <SignificantAnimation>[];
    bool stop = false;

    void visit(Element element) {
      if (stop) return;
      final Widget widget = element.widget;
      if (widget is TickerMode && !widget.enabled) return;
      if (widget is Offstage && widget.offstage) return;

      final Animation<Object?>? animation = _extractRunningAnimation(
        element,
        widget,
      );
      // A scroll still moving (ballistic / spring-back on iOS) counts as a
      // significant animation: its sub-pixel motion keeps the semantics
      // hash stable, yet the Scrollable ignores taps on its children until
      // the activity ends (ScrollActivity.shouldIgnorePointer).
      if (element is StatefulElement &&
          element.state is ScrollableState &&
          (element.state as ScrollableState).position.isScrollingNotifier.value) {
        out.add(
          SignificantAnimation(
            widget: 'Scrollable',
            fraction: 1,
            status: AnimationStatus.forward,
          ),
        );
        if (firstOnly) {
          stop = true;
          return;
        }
      }
      if (animation != null) {
        final double? fraction = _visibleScreenFraction(
          element,
          screen,
          screenArea,
        );
        if (fraction != null && fraction >= threshold) {
          out.add(
            SignificantAnimation(
              widget: widget.runtimeType.toString(),
              fraction: fraction,
              status: animation.status,
            ),
          );
          if (firstOnly) {
            stop = true;
            return;
          }
        }
      }
      element.visitChildren(visit);
    }

    root.visitChildren(visit);
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
    final Rect rect = MatrixUtils.transformRect(
      ro.getTransformTo(null),
      Offset.zero & ro.size,
    ).intersect(screen);
    if (rect.width <= 0 || rect.height <= 0) return 0;
    return rect.width * rect.height / screenArea;
  }
}
