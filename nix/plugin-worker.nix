{
  lib,
  stdenv,
  fetchurl,
  nodejs_24,
  src,
  version,
}:
let
  lock = (builtins.fromJSON (builtins.readFile ../package-lock.json)).packages;
  nodePlatform = if stdenv.hostPlatform.isDarwin then "darwin" else "linux";
  nodeArch = if stdenv.hostPlatform.isAarch64 then "arm64" else "x64";
  compiler = "@esbuild/${nodePlatform}-${nodeArch}";
  # Only SDK/worker build dependencies, not the old server's npm closure.
  sdkDependencies = lib.concatMap (workspace:
    map (name: { key = resolve "packages/${workspace}" name; })
      (builtins.filter (name: !(lib.hasPrefix "@getpaseo/" name))
        (builtins.attrNames ((builtins.fromJSON (builtins.readFile
          (../packages + "/${workspace}/package.json"))).dependencies or { })))
  ) [ "plugin" "protocol" "client" "relay" ];
  resolve = parent: name:
    let
      parts = lib.splitString "/" parent;
      candidates = map (length:
        let prefix = lib.concatStringsSep "/" (lib.take length parts);
        in lib.optionalString (prefix != "") "${prefix}/" + "node_modules/${name}"
      ) (lib.reverseList (lib.range 0 (builtins.length parts)));
    in lib.findFirst (key: builtins.hasAttr key lock)
      (throw "Missing locked optional-worker dependency ${name} from ${parent}") candidates;
  dependencies = builtins.genericClosure {
    startSet = (map (name: { key = "node_modules/${name}"; })
      [ "esbuild" "typescript" "zod" compiler ]) ++ sdkDependencies;
    operator = entry: map (name: { key = resolve entry.key name; })
      (builtins.attrNames (lock.${entry.key}.dependencies or { }));
  };
  unpack = entry:
    let
      dependency = lock.${entry.key};
      tarball = fetchurl { url = dependency.resolved; hash = dependency.integrity; };
    in ''
      mkdir -p ${lib.escapeShellArg entry.key}
      tar -xzf ${tarball} --strip-components=1 -C ${lib.escapeShellArg entry.key}
    '';
in
stdenv.mkDerivation {
  pname = "paseo-plugin-worker";
  inherit version src;
  nativeBuildInputs = [ nodejs_24 ];
  # Leave optional compiler JS shebangs portable. Patching them would retain
  # build-time Node in an otherwise Node-free daemon runtime closure.
  dontPatchShebangs = true;
  dontConfigure = true;
  buildPhase = ''
    runHook preBuild
    ${lib.concatMapStringsSep "\n" unpack dependencies}
    mkdir -p node_modules/@getpaseo
    ln -s ../../packages/plugin node_modules/@getpaseo/plugin
    node scripts/build-plugin-worker.mjs "$TMPDIR/plugin-worker"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    cp -R "$TMPDIR/plugin-worker" "$out"
    runHook postInstall
  '';
  disallowedRequisites = [ nodejs_24 ];
}
