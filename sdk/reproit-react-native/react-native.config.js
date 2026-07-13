module.exports = {
  dependency: {
    platforms: {
      android: {
        sourceDir: './android',
        packageImportPath: 'import com.reproit.reactnative.ReproItRuntimePackage;',
        packageInstance: 'new ReproItRuntimePackage()',
      },
      ios: { podspecPath: './reproit-react-native.podspec' },
    },
  },
};
