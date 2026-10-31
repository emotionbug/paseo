{
  lib,
  stdenv,
  rustPlatform,
  callPackage,
  fetchurl,
  nodejs_24,
  autoPatchelfHook,
  patchelf,
  darwin,
  sourceSha ? "",
}:
let
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).workspace.package.version;
  nodePlatform = if stdenv.hostPlatform.isDarwin then "darwin" else "linux";
  nodeArch = if stdenv.hostPlatform.isAarch64 then "arm64" else "x64";
  # Shared with desktop/standalone packaging: official release metadata hashes.
  sherpaAssets = builtins.fromJSON (builtins.readFile
    ../packages/flutter_desktop_host/packaging/sherpa-assets.json);
  sherpaAsset = sherpaAssets."${nodePlatform}-${nodeArch}";
  sherpaArchive = fetchurl {
    url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.28/${builtins.elemAt sherpaAsset 0}";
    sha256 = builtins.elemAt sherpaAsset 1;
  };
  src = lib.cleanSourceWith {
    src = ./..;
    filter = path: type:
      let
        name = builtins.baseNameOf path;
        relative = lib.removePrefix (toString ./.. + "/") (toString path);
        roots = [
          "Cargo.toml" "Cargo.lock" "package.json" "package-lock.json" "LICENSE"
          "tsconfig.json" "tsconfig.base.json" "skills" "plugins"
          "packages/daemon" "packages/protocol" "packages/plugin"
          "packages/client" "packages/relay"
          "packages/flutter_desktop_host/packaging" "scripts/build-plugin-worker.mjs"
        ];
      in lib.cleanSourceFilter path type
        && !(builtins.elem name [ "node_modules" "target" "dist" ".tmp" ".paseo" ])
        && (toString path == toString ./.. || lib.any (root:
          relative == root || lib.hasPrefix (root + "/") relative
          || (type == "directory" && lib.hasPrefix (relative + "/") root)) roots);
  };
  pluginWorker = callPackage ./plugin-worker.nix { inherit src version; };
in
rustPlatform.buildRustPackage {
  pname = "paseo";
  inherit version src;
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [ "--package" "paseo-daemon" "--bin" "paseo" ];
  # Integration tests need writable homes/provider fixtures and run separately.
  doCheck = false;
  nativeBuildInputs = [ nodejs_24 ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook patchelf ]
    ++ lib.optionals stdenv.hostPlatform.isDarwin [ darwin.cctools ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ stdenv.cc.cc.lib ];
  dontPatchShebangs = true;
  env.PASEO_BUILD_SOURCE_SHA = sourceSha;
  # Deliberately do not set PASEO_NATIVE_RELEASE or updater repository/channel:
  # only Nix may replace this store-managed installation.
  postInstall = ''
    mkdir -p "$out/lib/paseo"
    mv "$out/bin/paseo" "$out/lib/paseo/paseo"
    ln -s ../lib/paseo/paseo "$out/bin/paseo"
    cp -R skills "$out/lib/paseo/skills"
    cp -R ${pluginWorker} "$out/lib/paseo/plugin-worker"
    chmod -R u+w "$out/lib/paseo/plugin-worker"
    export XDG_CACHE_HOME="$TMPDIR/cache"
    mkdir -p "$XDG_CACHE_HOME/paseo-flutter-packaging/sherpa-onnx-1.12.28"
    cp ${sherpaArchive} "$XDG_CACHE_HOME/paseo-flutter-packaging/sherpa-onnx-1.12.28/${builtins.elemAt sherpaAsset 0}"
    node --input-type=module <<'JS'
    import fs from 'node:fs';
    import { copySherpaRuntime } from './packages/flutter_desktop_host/packaging/sherpa-runtime.mjs';
    import { cargoMetadata, collectCargoNotices, writeNotices, nativeThirdPartyNotices } from './packages/flutter_desktop_host/packaging/third-party-notices.mjs';
    const runtime = process.env.out + '/lib/paseo';
    await copySherpaRuntime(runtime);
    fs.copyFileSync('LICENSE', runtime + '/LICENSE');
    writeNotices(runtime, collectCargoNotices(cargoMetadata()), 'RUST-THIRD-PARTY-NOTICES');
    fs.writeFileSync(runtime + '/THIRD-PARTY-NOTICES.txt', nativeThirdPartyNotices(runtime));
    fs.writeFileSync(runtime + '/paseo-runtime.json', JSON.stringify({
      schemaVersion: 1, runtime: 'rust', appId: 'sh.paseo.daemon',
      installationKind: 'nix', release: false, version: '${version}',
      sourceSha: process.env.PASEO_BUILD_SOURCE_SHA,
      platform: '${if stdenv.hostPlatform.isDarwin then "macos" else "linux"}', arch: '${nodeArch}'
    }) + '\n');
    JS
  '';
  # The worker compiler is bundled, but an optional extension must explicitly
  # find externally installed Node via PATH or PASEO_PLUGIN_NODE.
  disallowedRequisites = [ nodejs_24 ];
  meta = {
    description = "Native self-hosted daemon for Claude Code, Codex, and OpenCode";
    homepage = "https://github.com/getpaseo/paseo";
    license = lib.licenses.asl20;
    mainProgram = "paseo";
    platforms = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
  };
}
