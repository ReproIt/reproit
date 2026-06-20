/** @type {import('ts-jest').JestConfigWithTsJest} */
module.exports = {
  preset: 'ts-jest',
  testEnvironment: 'node',
  testMatch: ['<rootDir>/test/**/*.test.ts'],
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
