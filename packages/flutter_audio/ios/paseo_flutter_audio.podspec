Pod::Spec.new do |s|
  s.name = 'paseo_flutter_audio'
  s.version = '1.0.0'
  s.summary = 'Paseo native audio playback engine.'
  s.description = 'AVAudioEngine PCM16 playback with AVAudioSession playback-category ownership.'
  s.homepage = 'https://github.com/getpaseo/paseo'
  s.license = { :type => 'MIT', :file => '../LICENSE' }
  s.author = { 'Paseo' => 'support@paseo.sh' }
  s.source = { :path => '.' }
  s.source_files = 'Classes/**/*'
  s.dependency 'Flutter'
  s.platform = :ios, '13.0'
  s.swift_version = '5.0'
  s.frameworks = 'AVFoundation'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
end
