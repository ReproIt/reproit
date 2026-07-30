/**
 * Deterministic stand-in for the react-native package in unit tests.
 *
 * The real package ships Flow-typed ESM that Jest cannot parse without the
 * React Native preset, and which resolution condition wins varies by npm and
 * Node version, so a suite that merely imports the SDK entry point passed
 * locally and failed on CI. Tests that need specific native module state
 * still override this with their own jest.mock factory, which takes
 * precedence over this mapping.
 */
export const NativeModules: Record<string, unknown> = {};
export const Platform = { OS: 'ios', Version: '0' };
export const AppState = { addEventListener: () => ({ remove: () => {} }), currentState: 'active' };
