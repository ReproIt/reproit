require 'json'
package = JSON.parse(File.read(File.join(__dir__, 'package.json')))
Pod::Spec.new do |s|
  s.name = 'ReproItReactNative'
  s.version = package['version']
  s.summary = package['description']
  s.homepage = 'https://reproit.com'
  s.license = package['license']
  s.author = package['author']
  s.platforms = { :ios => '12.0' }
  s.source = { :git => 'https://github.com/ReproIt/reproit.git', :tag => "v#{s.version}" }
  s.source_files = 'ios/**/*.{h,m,mm}'
  s.dependency 'React-Core'
end
