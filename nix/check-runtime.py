import ctypes
import json
from pathlib import Path
import subprocess
import sys
import tempfile

package = Path(sys.argv[1])
version = sys.argv[2]
runtime = package / "lib/paseo"
executable = package / "bin/paseo"
assert executable.resolve() == runtime / "paseo"
magic = executable.read_bytes()[:4]
assert magic in (b"\x7fELF", b"\xcf\xfa\xed\xfe"), "daemon must be a native binary, not a wrapper"
marker = json.loads((runtime / "paseo-runtime.json").read_text())
assert marker["runtime"] == "rust" and marker["installationKind"] == "nix"
assert marker["release"] is False and marker["version"] == version
assert list((runtime / "skills").rglob("SKILL.md")), "missing bundled skills"
assert "Apache License" in (runtime / "LICENSE").read_text()
assert "POSSIBILITY OF SUCH DAMAGE." in (runtime / "THIRD-PARTY-NOTICES.txt").read_text()

library_name = "libsherpa-onnx-c-api.dylib" if sys.platform == "darwin" else "libsherpa-onnx-c-api.so"
library = ctypes.CDLL(str(runtime / "lib" / library_name))
library.SherpaOnnxGetVersionStr.restype = ctypes.c_char_p
assert library.SherpaOnnxGetVersionStr() == b"1.12.28"
assert list((runtime / "lib").glob("*onnxruntime*")), "missing ONNX Runtime"
assert list((runtime / "lib/licenses").rglob("*")), "missing speech dependency notices"

worker = runtime / "plugin-worker"
assert (worker / "worker.mjs").is_file()
provenance = json.loads((worker / "paseo-provenance.json").read_text())
assert provenance["runtime"] == "external-node"
compiler_platform = provenance["platform"] + "-" + provenance["arch"]
assert sorted(path.name for path in (worker / "node_modules/@esbuild").iterdir()) == [compiler_platform]
compiler = worker / "node_modules/@esbuild" / compiler_platform / "bin/esbuild"
assert (worker / "node_modules/typescript/lib/typescript.js").is_file()
compiler_version = json.loads((worker / "node_modules/esbuild/package.json").read_text())["version"]
with tempfile.TemporaryDirectory() as home:
    # No Node (or any other external executable) is reachable through PATH.
    env = {"HOME": home, "PASEO_HOME": home, "PATH": ""}
    assert subprocess.check_output([compiler, "--version"], env=env, text=True).strip() == compiler_version
    output = subprocess.check_output([executable, "--version"], env=env, text=True)
    assert version in output
