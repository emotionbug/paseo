Pod::Spec.new do |s|
  s.name = 'paseo_desktop_integrations'
  s.version = '1.0.0'
  s.summary = 'Paseo native desktop integration services.'
  s.description = s.summary
  s.homepage = 'https://paseo.sh'
  s.license = { :type => 'Apache-2.0' }
  s.author = 'Paseo'
  s.source = { :path => '.' }
  s.source_files = 'Classes/**/*'
  s.dependency 'FlutterMacOS'
  s.platform = :osx, '10.15'
  s.swift_version = '5.0'
  s.frameworks = 'Cocoa', 'CoreGraphics'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
end
