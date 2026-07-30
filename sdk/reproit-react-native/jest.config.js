/** @type {import('ts-jest').JestConfigWithTsJest} */
module.exports = {
  preset: 'ts-jest',
  testEnvironment: 'node',
  testMatch: ['<rootDir>/test/**/*.test.ts'],
  // Pin react-native resolution: the real package is Flow-typed ESM whose
  // winning export condition differs across npm and Node versions, so a test
  // that imports the SDK entry point must not depend on it. Tests needing
  // specific native state still supply their own jest.mock factory.
  moduleNameMapper: {
    '^react-native$': '<rootDir>/test/stubs/react-native.ts',
  },
  transform: {
    '^.+\\.tsx?$': [
      'ts-jest',
      {
        // The parity test only touches pure modules (signature, snapshot),
        // so it doesn't need react / react-native installed.
        tsconfig: {
          jsx: 'react',
          esModuleInterop: true,
          skipLibCheck: true,
        },
      },
    ],
  },
};
