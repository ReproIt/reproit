/**
 * <ReproItProvider>, optional wrapper that improves capture fidelity.
 *
 * It does two things, both optional and additive on top of `ReproIt.init`:
 *
 *  1. Tap labelling. It puts a transparent responder at the root of your tree
 *     (`onStartShouldSetResponderCapture`) so it sees every touch-down without
 *     stealing the gesture from your buttons. On each touch it asks the SDK for
 *     the current tappable labels and (best-effort) labels the next edge
 *     `tap:<label>`. RN gives a library no synchronous hit-test by screen
 *     point, so when several tappables are present the label is a best-effort
 *     pick (see README "Limitations"); the resulting STATE signatures are still
 *     exact because they come from the fiber snapshot, not the tap.
 *
 *  2. Navigation labelling. Pass a React Navigation `navigationRef` and route
 *     changes are recorded as `nav:<routeName>`, matching the runner/Flutter
 *     `nav:` edges.
 *
 * No app-code changes are required beyond `ReproIt.init` + wrapping your tree
 * (and optionally passing `navigationRef`). react-navigation is NOT a
 * dependency; the prop is duck-typed.
 */
import * as React from 'react';
import { View } from 'react-native';
import { ReproIt } from './index';

/** Minimal duck-typed shape of a React Navigation container ref. */
export interface NavigationRefLike {
  addListener?: (type: string, cb: () => void) => () => void;
  getCurrentRoute?: () => { name?: string } | undefined;
}

export interface ReproItProviderProps {
  children?: React.ReactNode;
  /** Optional React Navigation `navigationRef` for `nav:<route>` edges. */
  navigationRef?: NavigationRefLike | null;
}

export function ReproItProvider(props: ReproItProviderProps): React.ReactElement {
  const { children, navigationRef } = props;

  // Subscribe to navigation route changes when a ref is supplied.
  React.useEffect(() => {
    if (!navigationRef || typeof navigationRef.addListener !== 'function') {
      return undefined;
    }
    const fire = () => {
      const route = navigationRef.getCurrentRoute?.();
      ReproIt.noteRoute(route?.name ?? null);
    };
    // Record the current route immediately, then on every change.
    fire();
    const unsub = navigationRef.addListener('state', fire);
    return typeof unsub === 'function' ? unsub : undefined;
  }, [navigationRef]);

  const onTouchCapture = React.useCallback((): boolean => {
    // Best-effort tap label: pick from current tappables. When there is exactly
    // one tappable on screen this is exact; otherwise it is the first by tree
    // order (documented limitation). The edge's STATE signature is unaffected.
    const labels = ReproIt.tappableLabels();
    ReproIt.noteTapLabel(labels.length ? labels[0] : null);
    return false; // never become the responder; let the touch pass through
  }, []);

  return React.createElement(
    View,
    {
      style: { flex: 1 },
      onStartShouldSetResponderCapture: onTouchCapture,
      // also fire on move-start so taps that begin as scrolls are still seen
      onMoveShouldSetResponderCapture: () => false,
    },
    children
  );
}

export default ReproItProvider;
