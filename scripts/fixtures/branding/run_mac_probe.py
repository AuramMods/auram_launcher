"""Run the native GLFW probe through a packaged Auram game helper on macOS."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time
from zipfile import ZipFile


PROBE = Path(__file__).with_name("AuramProbe.java")
PAYLOAD = 'Unicode 雪 with spaces, quotes " and backslash \\'


def token(value):
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


def run(helper, java, libraries, branding):
    with tempfile.TemporaryDirectory(prefix="Auram 世界 ") as directory:
        root = Path(directory)
        target = root / "classes"
        target.mkdir()
        glfw = root / "libglfw.dylib"
        with ZipFile(next(libraries.glob("lwjgl-glfw-*-natives-macos-arm64.jar"))) as archive:
            glfw.write_bytes(archive.read("macos/arm64/org/lwjgl/glfw/libglfw.dylib"))
        jars = ":".join(str(path) for path in sorted(libraries.glob("*.jar")))
        subprocess.run([java, "-m", "jdk.compiler/com.sun.tools.javac.Main", "-encoding", "UTF-8",
                        "-cp", jars, "-d", target, PROBE], check=True)
        icon = branding / "icon.png"
        for mode in ("branded", "absent", "broken"):
            report = root / f"{mode} report 雪.txt"
            close = root / f"{mode}.close"
            args = ["-XstartOnFirstThread", f"-Dorg.lwjgl.glfw.libname={glfw}"]
            if mode != "absent":
                agent = branding / ("missing agent.jar" if mode == "broken" else "auram-bootstrap.jar")
                args.extend(["-Dauram.branding.name=Auram", f"-Dauram.branding.icon={icon}",
                             f"-javaagent:{agent}"])
            argfile = root / f"{mode} JVM arguments 雪.txt"
            argfile.write_text("\n".join(map(token, [*args, "-cp", f"{target}:{jars}", "AuramProbe"])) + "\n")
            invocation = {
                "library": str(java.parent.parent / "lib/libjli.dylib"),
                "glfw": str(glfw),
                "arguments": [f"@{argfile}", str(report), str(close), str(mode != "absent").lower(),
                              PAYLOAD, str(icon)],
            }
            with subprocess.Popen([helper, "--auram-game-pipe"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=root) as game:
                game.stdin.write(json.dumps(invocation).encode())
                game.stdin.close()
                try:
                    for _ in range(150):
                        if game.poll() is not None or report.is_file():
                            break
                        time.sleep(0.1)
                    close.touch()
                    game.wait(timeout=10)
                finally:
                    if game.poll() is None:
                        game.kill()
                out = game.stdout.read().decode(errors="replace")
                err = game.stderr.read().decode(errors="replace")
                print(mode, game.pid, game.returncode, out, err, flush=True)
                if mode == "broken":
                    assert game.returncode != 0 and "Error opening zip file" in err
                else:
                    result = report.read_text()
                    print(result, flush=True)
                    assert game.returncode == 23 and f"pid={game.pid}" in result
                    assert "probe stdout" in out and "probe stderr" in err


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path, help="Auram.app/Contents/MacOS/Auram game executable")
    parser.add_argument("java", type=Path)
    parser.add_argument("libraries", type=Path, help="LWJGL, macOS ARM64 natives, and JNA jars")
    parser.add_argument("branding", type=Path)
    args = parser.parse_args()
    run(*(path.resolve() for path in (args.helper, args.java, args.libraries, args.branding)))
