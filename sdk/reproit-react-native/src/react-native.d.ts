/**
 * Minimal ambient declaration for the slice of `react-native` this SDK uses.
 *
 * This lets the package typecheck and emit `.d.ts` standalone, without the full
 * React Native toolchain installed (it is a peerDependency). When the SDK is
 * consumed inside a real RN app, that app's own `react-native` types are
 * present and authoritative; this shim only covers `View` + the responder
 * props the provider attaches, and is intentionally tiny.
 */
declare module 'react-native' {
  import type * as React from 'react';

  export interface GestureResponderEvent {
    nativeEvent: {
      locationX: number;
      locationY: number;
      pageX: number;
      pageY: number;
      [key: string]: unknown;
    };
  }

  export interface ViewProps {
    style?: unknown;
    children?: React.ReactNode;
    onStartShouldSetResponderCapture?: (e: GestureResponderEvent) => boolean;
    onMoveShouldSetResponderCapture?: (e: GestureResponderEvent) => boolean;
    [key: string]: unknown;
  }

  export const View: React.ComponentType<ViewProps>;

  /**
   * The RN `Platform` module. Only the fields the context collector reads are
   * declared here; the consuming app's own `react-native` types are
   * authoritative when the SDK runs inside a real app.
   */
  export const Platform: {
    OS: string;
    Version?: string | number;
    [key: string]: unknown;
  };
}
